use serde_json::Value;
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

fn agent_json(output: std::process::Output) -> Result<Value, Box<dyn std::error::Error>> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn first_table_id(path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = docsight().args(["--agent", "overview", path]).output()?;
    let value = agent_json(output)?;
    let landmarks = value["result"]["landmarks"].as_array().ok_or("landmarks")?;
    landmarks
        .iter()
        .find(|landmark| landmark["kind"] == "table")
        .and_then(|landmark| landmark["id"].as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "missing table landmark".into())
}

#[test]
fn overview_and_focus_expose_bounded_semantic_navigation() -> Result<(), Box<dyn std::error::Error>>
{
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("invalid path")?;
    let table = first_table_id(path)?;

    let overview = agent_json(
        docsight()
            .args(["--agent", "--max-items", "2", "overview", path])
            .output()?,
    )?;
    assert_eq!(overview["schema"], "docsight.agent/v2");
    assert_eq!(overview["result"]["total_landmarks"], 6);
    assert_eq!(overview["limits"]["truncated"], true);
    assert_eq!(overview["limits"]["returned_items"], 2);
    assert!(
        overview["result"]["landmarks"][0]["text_snippet"]
            .as_str()
            .is_some()
    );

    let text_limited = agent_json(
        docsight()
            .args([
                "--agent",
                "--max-items",
                "1",
                "--text-limit",
                "10",
                "overview",
                path,
            ])
            .output()?,
    )?;
    assert_eq!(text_limited["limits"]["text_truncated"], true);
    assert!(
        text_limited["result"]["landmarks"][0]["text_snippet"]
            .as_str()
            .ok_or("snippet")?
            .chars()
            .count()
            <= 10
    );

    let focus = agent_json(
        docsight()
            .args(["--agent", "focus", path, table.as_str(), "--related"])
            .output()?,
    )?;
    assert_eq!(focus["result"]["target"]["kind"], "object");
    assert_eq!(focus["result"]["target"]["id"], table);
    assert!(
        focus["result"]["objects"]
            .as_array()
            .ok_or("objects")?
            .iter()
            .any(|object| {
                object["object"]["id"] == table
                    && object["relationships"]
                        .as_array()
                        .is_some_and(|relationships| {
                            relationships
                                .iter()
                                .any(|relationship| relationship["role"] == "target")
                        })
            })
    );
    assert_eq!(focus["result"]["visual_references"][0]["command"], "crop");
    assert_eq!(
        focus["result"]["visual_references"][0]["artifact_available"],
        false
    );

    let page_focus = agent_json(
        docsight()
            .args(["--agent", "focus", path, "--pages", "1..1"])
            .output()?,
    )?;
    assert_eq!(page_focus["result"]["target"]["kind"], "page_range");
    assert_eq!(page_focus["result"]["target"]["start_page"], 1);
    assert_eq!(page_focus["result"]["target"]["end_page"], 1);
    assert!(
        page_focus["result"]["objects"]
            .as_array()
            .ok_or("page objects")?
            .iter()
            .any(|object| {
                object["relationships"]
                    .as_array()
                    .is_some_and(|relationships| {
                        relationships
                            .iter()
                            .any(|relationship| relationship["role"] == "next")
                    })
            })
    );
    Ok(())
}

#[test]
fn spatial_query_is_point_bounded_and_resumable() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("invalid path")?;
    let table = first_table_id(path)?;
    let expression = format!("object distance-to({table}) < 100pt");

    let first = agent_json(
        docsight()
            .args([
                "--agent",
                "--max-items",
                "2",
                "query",
                path,
                expression.as_str(),
            ])
            .output()?,
    )?;
    assert_eq!(first["result"]["total_matches"], 9);
    assert_eq!(
        first["result"]["matches"]
            .as_array()
            .ok_or("matches")?
            .len(),
        2
    );
    assert_eq!(
        first["result"]["matches"][0]["relation"]["kind"],
        "distance_to"
    );
    let token = first["limits"]["continuation_token"]
        .as_str()
        .ok_or("missing continuation token")?
        .to_owned();
    let first_id = first["result"]["matches"][0]["object"]["id"]
        .as_str()
        .ok_or("first id")?
        .to_owned();

    let resumed = agent_json(
        docsight()
            .args([
                "--agent",
                "--max-items",
                "2",
                "--continue",
                token.as_str(),
                "query",
                path,
                expression.as_str(),
            ])
            .output()?,
    )?;
    assert_ne!(resumed["result"]["matches"][0]["object"]["id"], first_id);
    assert_eq!(resumed["result"]["total_matches"], 9);

    let other_expression = format!("object distance-to({table}) < 90pt");
    let mismatched = docsight()
        .args([
            "--agent",
            "--continue",
            token.as_str(),
            "query",
            path,
            other_expression.as_str(),
        ])
        .output()?;
    assert_eq!(mismatched.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&mismatched.stderr)?;
    assert_eq!(error["error"]["code"], "USAGE");
    Ok(())
}

#[test]
fn ndjson_and_invalid_units_keep_the_agent_contract() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("invalid path")?;
    let table = first_table_id(path)?;
    let expression = format!("object distance-to({table}) < 100pt");
    let output = docsight()
        .args([
            "--agent",
            "--ndjson",
            "--max-items",
            "2",
            "query",
            path,
            expression.as_str(),
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
    assert_eq!(records[1]["type"], "query.summary");
    assert_eq!(records[2]["type"], "query.match");
    assert_eq!(records.last().ok_or("done")?["type"], "done");

    let invalid = docsight()
        .args([
            "--agent",
            "query",
            path,
            format!("object distance-to({table}) < 24px").as_str(),
        ])
        .output()?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    let error: Value = serde_json::from_slice(&invalid.stderr)?;
    assert_eq!(error["schema"], "docsight.agent/v2");
    assert_eq!(error["error"]["code"], "USAGE");

    let pdf = fixture("sample_semantic.pdf");
    let pdf = pdf.to_str().ok_or("invalid PDF path")?;
    let unsupported = docsight()
        .args(["--agent", "query", pdf, "figure nearest(caption)"])
        .output()?;
    assert_eq!(unsupported.status.code(), Some(20));
    assert!(unsupported.stdout.is_empty());
    let error: Value = serde_json::from_slice(&unsupported.stderr)?;
    assert_eq!(error["error"]["code"], "LAYOUT_PARTIAL");
    Ok(())
}

#[test]
fn m12_agent_outputs_are_byte_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("invalid path")?;
    let table = first_table_id(path)?;
    let expression = format!("object distance-to({table}) < 100pt");
    let args = [
        "--agent",
        "--max-items",
        "3",
        "query",
        path,
        expression.as_str(),
    ];
    let first = docsight().args(args).output()?;
    let second = docsight().args(args).output()?;
    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stderr, second.stderr);
    Ok(())
}
