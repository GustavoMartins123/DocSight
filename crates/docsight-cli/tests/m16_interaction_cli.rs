use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("fixtures").join("validation").join(name)
}

fn agent_json(args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let output = docsight().args(args).output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn peek_is_compact_bounded_resumable_and_section_addressable()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let first = agent_json(&["--agent", "--max-items", "2", "peek", path, "--page", "1"])?;
    assert_eq!(first["result"]["target"]["kind"], "page");
    assert_eq!(first["result"]["scope_pages"], serde_json::json!([1]));
    assert_eq!(first["limits"]["truncated"], true);
    assert_eq!(first["limits"]["returned_items"], 2);
    let objects = first["result"]["objects"].as_array().ok_or("objects")?;
    assert_eq!(objects.len(), 2);
    assert!(objects.iter().all(|entry| entry.get("content").is_none()));
    assert!(objects.iter().all(|entry| {
        entry["object"]["text_snippet"]
            .as_str()
            .is_some_and(|text| text.chars().count() <= 240)
    }));
    let first_id = objects[0]["object"]["id"]
        .as_str()
        .ok_or("first object")?
        .to_owned();
    let token = first["limits"]["continuation_token"]
        .as_str()
        .ok_or("continuation token")?
        .to_owned();
    let resumed = agent_json(&[
        "--agent",
        "--max-items",
        "2",
        "--continue",
        token.as_str(),
        "peek",
        path,
        "--page",
        "1",
    ])?;
    assert_ne!(resumed["result"]["objects"][0]["object"]["id"], first_id);

    let section = agent_json(&[
        "--agent",
        "--max-items",
        "1",
        "peek",
        path,
        "--section",
        "1",
    ])?;
    assert_eq!(section["result"]["target"]["kind"], "section");
    assert_eq!(section["result"]["target"]["index"], 1);
    assert!(
        section["result"]["target"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("sect_"))
    );
    Ok(())
}

#[test]
fn resolve_is_explainable_deterministic_and_ambiguity_preserving()
-> Result<(), Box<dyn std::error::Error>> {
    let headings = fixture("sample_headings.docx");
    let headings = headings.to_str().ok_or("headings path")?;
    let args = [
        "--agent",
        "resolve",
        headings,
        "--text",
        "Application Architecture Guide",
        "--kind",
        "heading",
    ];
    let first = agent_json(&args)?;
    let second = agent_json(&args)?;
    assert_eq!(first, second);
    assert_eq!(first["result"]["status"], "resolved");
    assert_eq!(
        first["result"]["candidates"][0]["object"]["id"],
        "h_515ad605791c12fc496c1c18d79f6526"
    );
    assert_eq!(
        first["result"]["candidates"][0]["matched_range"]["text"],
        "Application Architecture Guide"
    );
    assert!(
        first["result"]["candidates"][0]["reasons"]
            .as_array()
            .ok_or("reasons")?
            .iter()
            .any(|reason| {
                reason["code"] == "direct_text" && reason["score"] == 1.0 && reason["weight"] == 0.5
            })
    );

    let tables = fixture("sample_tables.docx");
    let tables = tables.to_str().ok_or("tables path")?;
    let ambiguous = agent_json(&[
        "--agent", "resolve", tables, "--text", "C1", "--kind", "table",
    ])?;
    assert_eq!(ambiguous["result"]["status"], "ambiguous");
    assert!(
        ambiguous["result"]["total_candidates"]
            .as_u64()
            .unwrap_or(0)
            >= 2
    );

    let absent = agent_json(&[
        "--agent",
        "resolve",
        tables,
        "--text",
        "descriptor-that-does-not-exist",
        "--kind",
        "table",
    ])?;
    assert_eq!(absent["result"]["status"], "no_match");
    assert_eq!(absent["result"]["total_candidates"], 0);
    Ok(())
}

#[test]
fn context_aggregates_full_typed_evidence_and_one_call_find()
-> Result<(), Box<dyn std::error::Error>> {
    let tables = fixture("sample_tables.docx");
    let tables = tables.to_str().ok_or("tables path")?;
    let table_id = "tbl_6c22c8dd17adf7e32604b29241d9ab99";
    let explicit = agent_json(&[
        "--agent",
        "context",
        tables,
        table_id,
        "--include",
        "content,neighbors,geometry,fidelity,provenance,heading",
    ])?;
    assert_eq!(explicit["result"]["status"], "resolved");
    assert_eq!(explicit["result"]["selection"]["mode"], "explicit_object");
    assert_eq!(explicit["result"]["context"]["target"]["id"], table_id);
    assert_eq!(explicit["result"]["context"]["content"]["type"], "table");
    assert!(
        explicit["result"]["context"]["content"]["cells"]
            .as_array()
            .is_some_and(|cells| !cells.is_empty())
    );
    assert!(explicit["result"]["context"]["geometry"]["available"] == true);
    assert!(explicit["result"]["context"]["fidelity"]["available"] == true);
    assert_eq!(
        explicit["result"]["context"]["provenance"]["source_path"],
        "/word/document.xml::body/tbl[1]"
    );
    assert_eq!(
        explicit["result"]["context"]["containers"]["section_status"],
        "exact"
    );
    assert_eq!(
        explicit["result"]["context"]["neighbors"]
            .as_array()
            .ok_or("neighbors")?
            .len(),
        2
    );

    let headings = fixture("sample_headings.docx");
    let headings = headings.to_str().ok_or("headings path")?;
    let found = agent_json(&[
        "--agent",
        "context",
        headings,
        "--find",
        "Application-Architecture Guide",
        "--kind",
        "heading",
        "--include",
        "content,geometry,provenance",
    ])?;
    assert_eq!(found["result"]["status"], "resolved");
    assert_eq!(found["result"]["selection"]["mode"], "find");
    assert_eq!(
        found["result"]["selection"]["chosen_object"],
        "h_515ad605791c12fc496c1c18d79f6526"
    );
    assert_eq!(
        found["result"]["selection"]["matched_range"]["text"],
        "Application Architecture Guide"
    );
    assert!(found["result"]["context"].is_object());
    assert!(found["result"]["context"].get("fidelity").is_none());
    Ok(())
}

#[test]
fn interaction_ndjson_errors_capabilities_and_schemas_are_typed()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let output = docsight()
        .args([
            "--agent",
            "--ndjson",
            "--max-items",
            "2",
            "resolve",
            path,
            "--text",
            "Application Architecture Guide",
        ])
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Value>, _>>()?;
    assert_eq!(records[0]["type"], "meta");
    assert_eq!(records[1]["type"], "resolve.summary");
    assert_eq!(records[2]["type"], "resolve.candidate");
    assert_eq!(records.last().ok_or("done")?["type"], "done");

    let invalid = docsight()
        .args([
            "--agent",
            "peek",
            path,
            "--page",
            "1",
            "--object",
            "h_515ad605791c12fc496c1c18d79f6526",
        ])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    let error: Value = serde_json::from_slice(&invalid.stderr)?;
    assert_eq!(error["error"]["code"], "USAGE");

    let capabilities = agent_json(&["--agent", "capabilities"])?;
    let command_names = capabilities["result"]["commands"]
        .as_array()
        .ok_or("commands")?
        .iter()
        .filter_map(|command| command["name"].as_str())
        .collect::<Vec<_>>();
    for name in ["peek", "context", "resolve"] {
        assert!(command_names.contains(&name));
    }

    for schema in [
        "peek-result.json",
        "context-result.json",
        "resolve-result.json",
    ] {
        let value: Value = serde_json::from_slice(&std::fs::read(
            workspace().join("schemas").join("v2").join(schema),
        )?)?;
        assert_eq!(
            value["$id"],
            format!("https://docsight.dev/schemas/v2/{schema}")
        );
    }
    let events: Value = serde_json::from_slice(&std::fs::read(
        workspace()
            .join("schemas")
            .join("v2")
            .join("ndjson-event.json"),
    )?)?;
    let event_types = events["properties"]["type"]["enum"]
        .as_array()
        .ok_or("event types")?;
    for event in [
        "peek.summary",
        "peek.object",
        "context",
        "context.candidate",
        "resolve.summary",
        "resolve.candidate",
    ] {
        assert!(event_types.iter().any(|value| value == event));
    }
    Ok(())
}

#[test]
fn interaction_limits_fail_closed_at_declared_boundaries() -> Result<(), Box<dyn std::error::Error>>
{
    let specification = workspace().join("Projeto_DOCSIGHT_Especificacao.docx");
    let specification = specification.to_str().ok_or("specification path")?;
    let eight_pages = docsight()
        .args([
            "--agent",
            "--max-items",
            "1",
            "peek",
            specification,
            "--pages",
            "1..8",
        ])
        .output()?;
    assert!(eight_pages.status.success());
    assert!(eight_pages.stderr.is_empty());

    let nine_pages = docsight()
        .args(["--agent", "peek", specification, "--pages", "1..9"])
        .output()?;
    assert_eq!(nine_pages.status.code(), Some(13));
    assert!(nine_pages.stdout.is_empty());
    let error: Value = serde_json::from_slice(&nine_pages.stderr)?;
    assert_eq!(error["error"]["code"], "RESOURCE_LIMIT");

    let allowed = "a".repeat(4096);
    let allowed_output = docsight()
        .args([
            "--agent",
            "resolve",
            specification,
            "--text",
            allowed.as_str(),
        ])
        .output()?;
    assert!(allowed_output.status.success());
    assert!(allowed_output.stderr.is_empty());

    let rejected = "a".repeat(4097);
    let rejected_output = docsight()
        .args([
            "--agent",
            "resolve",
            specification,
            "--text",
            rejected.as_str(),
        ])
        .output()?;
    assert_eq!(rejected_output.status.code(), Some(13));
    assert!(rejected_output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&rejected_output.stderr)?;
    assert_eq!(error["error"]["code"], "RESOURCE_LIMIT");

    let zero_section = docsight()
        .args(["--agent", "peek", specification, "--section", "0"])
        .output()?;
    assert_eq!(zero_section.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&zero_section.stderr)?;
    assert_eq!(error["error"]["code"], "USAGE");

    let pdf = fixture("sample_semantic.pdf");
    let pdf = pdf.to_str().ok_or("PDF path")?;
    let pdf_section = docsight()
        .args(["--agent", "peek", pdf, "--section", "1"])
        .output()?;
    assert_eq!(pdf_section.status.code(), Some(20));
    let error: Value = serde_json::from_slice(&pdf_section.stderr)?;
    assert_eq!(error["error"]["code"], "LAYOUT_PARTIAL");
    Ok(())
}

#[test]
fn interaction_commands_run_through_the_sandbox() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let path = path.to_str().ok_or("fixture path")?;
    for arguments in [
        vec!["peek", path, "--page", "1"],
        vec![
            "context",
            path,
            "h_515ad605791c12fc496c1c18d79f6526",
            "--include",
            "geometry,provenance",
        ],
        vec![
            "resolve",
            path,
            "--text",
            "Application Architecture Guide",
            "--kind",
            "heading",
        ],
    ] {
        let output = docsight()
            .args(["--agent", "--sandbox"])
            .args(arguments)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["schema"], "docsight.agent/v2");
    }
    Ok(())
}
