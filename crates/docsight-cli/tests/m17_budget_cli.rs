use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::{Command, Output};

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

fn run(args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(docsight().args(args).output()?)
}

fn run_json(args: &[&str]) -> Result<(Output, Value), Box<dyn std::error::Error>> {
    let output = run(args)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value = serde_json::from_slice(&output.stdout)?;
    Ok((output, value))
}

fn context_args(path: &str) -> Vec<&str> {
    vec![
        "--agent",
        "context",
        path,
        "tbl_6c22c8dd17adf7e32604b29241d9ab99",
        "--include",
        "content,neighbors,geometry,fidelity,provenance,heading,related",
    ]
}

fn output_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn conformance_record(name: &str, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let first = run(args)?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stderr.is_empty());
    let repeated = run(args)?;
    assert_eq!(first.stdout, repeated.stdout);
    let value: Value = serde_json::from_slice(&first.stdout)?;
    Ok(serde_json::json!({
        "name": name,
        "stdout_bytes": first.stdout.len(),
        "stdout_sha256": output_digest(&first.stdout),
        "selected_profile": value["limits"]["projection"]["selected_profile"],
        "omitted_evidence": value["limits"]["projection"]["omitted_evidence"],
        "truncated": value["limits"]["truncated"],
        "returned_items": value["limits"].get("returned_items"),
        "total_items": value["limits"].get("total_items")
    }))
}

#[test]
fn fixed_profiles_form_strict_explainable_evidence_tiers() -> Result<(), Box<dyn std::error::Error>>
{
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let mut rich_args = context_args(path);
    rich_args.extend(["--budget-profile", "rich"]);
    let mut balanced_args = context_args(path);
    balanced_args.extend(["--budget-profile", "balanced"]);
    let mut compact_args = context_args(path);
    compact_args.extend(["--budget-profile", "compact"]);

    let (rich_output, rich) = run_json(&rich_args)?;
    let (balanced_output, balanced) = run_json(&balanced_args)?;
    let (compact_output, compact) = run_json(&compact_args)?;
    assert!(rich_output.stdout.len() > balanced_output.stdout.len());
    assert!(balanced_output.stdout.len() > compact_output.stdout.len());

    assert_eq!(rich["limits"]["projection"]["selected_profile"], "rich");
    assert_eq!(rich["limits"]["projection"]["adaptive"], false);
    assert!(rich["result"]["context"].get("content").is_some());
    assert!(rich["result"]["context"].get("related").is_some());

    assert_eq!(
        balanced["limits"]["projection"]["selected_profile"],
        "balanced"
    );
    assert!(balanced["result"]["context"].get("content").is_none());
    assert!(balanced["result"]["context"].get("related").is_none());
    assert!(balanced["result"]["context"].get("geometry").is_some());
    assert!(balanced["result"]["context"].get("fidelity").is_some());
    assert!(balanced["result"]["context"].get("provenance").is_some());
    assert!(balanced["result"]["context"].get("neighbors").is_some());

    assert_eq!(
        compact["limits"]["projection"]["selected_profile"],
        "compact"
    );
    assert!(compact["result"]["context"].get("content").is_none());
    assert!(compact["result"]["context"].get("geometry").is_none());
    assert!(compact["result"]["context"].get("fidelity").is_none());
    assert!(compact["result"]["context"].get("provenance").is_none());
    assert!(compact["result"]["context"].get("neighbors").is_none());
    assert_eq!(
        compact["result"]["selection"]["reasons"],
        serde_json::json!([])
    );
    let omissions = compact["limits"]["projection"]["omitted_evidence"]
        .as_array()
        .ok_or("omitted evidence")?;
    assert!(omissions.iter().any(|value| value == "full_content"));
    assert!(omissions.iter().any(|value| value == "geometry"));
    assert!(omissions.iter().any(|value| value == "ranking_components"));

    let repeated = run(&compact_args)?;
    assert_eq!(compact_output.stdout, repeated.stdout);
    Ok(())
}

#[test]
fn adaptive_budget_selects_richest_fitting_profile_in_serialized_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    for (budget, expected, bytes) in [
        ("24kb", "rich", 24 * 1024),
        ("4kb", "balanced", 4 * 1024),
        ("3kb", "compact", 3 * 1024),
    ] {
        let mut args = context_args(path);
        args.extend(["--budget", budget]);
        let (output, value) = run_json(&args)?;
        assert!(output.stdout.len() <= bytes);
        assert_eq!(value["limits"]["projection"]["selected_profile"], expected);
        assert_eq!(value["limits"]["projection"]["adaptive"], true);
        assert_eq!(value["limits"]["projection"]["budget_bytes"], bytes);
        assert_eq!(value["limits"]["truncated"], false);
        assert!(value["limits"].get("warnings_truncated").is_none());
        let repeated = run(&args)?;
        assert_eq!(output.stdout, repeated.stdout);
    }
    Ok(())
}

#[test]
fn collection_budget_uses_deterministic_continuation_after_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let args = ["--agent", "peek", path, "--page", "1", "--budget", "4kb"];
    let (first_output, first) = run_json(&args)?;
    assert!(first_output.stdout.len() <= 4 * 1024);
    assert_eq!(first["limits"]["projection"]["selected_profile"], "compact");
    assert_eq!(first["limits"]["truncated"], true);
    let returned = first["limits"]["returned_items"]
        .as_u64()
        .ok_or("returned items")?;
    let total = first["limits"]["total_items"]
        .as_u64()
        .ok_or("total items")?;
    assert!(returned > 0);
    assert!(returned < total);
    let first_id = first["result"]["objects"][0]["object"]["id"]
        .as_str()
        .ok_or("first id")?;
    let token = first["limits"]["continuation_token"]
        .as_str()
        .ok_or("continuation token")?;
    let resumed_args = [
        "--agent",
        "--continue",
        token,
        "peek",
        path,
        "--page",
        "1",
        "--budget",
        "4kb",
    ];
    let (resumed_output, resumed) = run_json(&resumed_args)?;
    assert!(resumed_output.stdout.len() <= 4 * 1024);
    assert_ne!(resumed["result"]["objects"][0]["object"]["id"], first_id);
    Ok(())
}

#[test]
fn invalid_or_unfulfillable_budget_requests_fail_closed() -> Result<(), Box<dyn std::error::Error>>
{
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let failures = [
        vec![
            "--agent",
            "context",
            path,
            "tbl_6c22c8dd17adf7e32604b29241d9ab99",
            "--budget",
            "2kb",
        ],
        vec![
            "--agent",
            "context",
            path,
            "tbl_6c22c8dd17adf7e32604b29241d9ab99",
            "--budget",
            "4kb",
            "--budget-profile",
            "rich",
        ],
        vec!["--agent", "peek", path, "--page", "1", "--budget", "0"],
        vec!["--agent", "peek", path, "--page", "1", "--budget", "4kib"],
        vec![
            "--agent", "--ndjson", "peek", path, "--page", "1", "--budget", "4kb",
        ],
        vec!["--agent", "capabilities", "--budget", "4kb"],
    ];
    for args in failures {
        let output = run(&args)?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr)?;
        assert_eq!(error["error"]["code"], "USAGE");
        assert_eq!(error["error"]["exit_code"], 2);
    }
    Ok(())
}

#[test]
fn budget_contract_is_discoverable_schema_bound_and_sandbox_safe()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let mut direct_args = context_args(path);
    direct_args.extend(["--budget", "4kb"]);
    let direct = run(&direct_args)?;
    let mut sandbox_args = context_args(path);
    sandbox_args.splice(1..1, ["--sandbox"]);
    sandbox_args.extend(["--budget", "4kb"]);
    let sandboxed = run(&sandbox_args)?;
    assert!(sandboxed.status.success());
    assert_eq!(direct.stdout, sandboxed.stdout);

    let (_, capabilities) = run_json(&["--agent", "capabilities"])?;
    let limits = capabilities["result"]["limits"]
        .as_array()
        .ok_or("capability limits")?;
    assert!(limits.iter().any(|limit| limit == "--budget"));
    assert!(limits.iter().any(|limit| limit == "--budget-profile"));

    let projection_schema: Value = serde_json::from_slice(&std::fs::read(
        workspace().join("schemas/v2/projection-selection.json"),
    )?)?;
    assert_eq!(
        projection_schema["$id"],
        "https://docsight.dev/schemas/v2/projection-selection.json"
    );
    let envelope_schema: Value = serde_json::from_slice(&std::fs::read(
        workspace().join("schemas/v2/agent-envelope.json"),
    )?)?;
    assert_eq!(
        envelope_schema["properties"]["limits"]["properties"]["projection"]["$ref"],
        "#/$defs/projection_selection"
    );
    assert!(envelope_schema["$defs"]["projection_selection"].is_object());
    Ok(())
}

#[test]
fn budget_envelopes_match_the_cross_platform_golden() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("fixture path")?;
    let mut rich = context_args(path);
    rich.extend(["--budget", "24kb"]);
    let mut balanced = context_args(path);
    balanced.extend(["--budget", "4kb"]);
    let mut compact = context_args(path);
    compact.extend(["--budget", "3kb"]);
    let peek = ["--agent", "peek", path, "--page", "1", "--budget", "4kb"];
    let pdf_path = fixture("sample_semantic.pdf");
    let pdf_path = pdf_path.to_str().ok_or("PDF fixture path")?;
    let pdf_peek = [
        "--agent", "peek", pdf_path, "--page", "1", "--budget", "3kb",
    ];
    let actual = serde_json::json!({
        "schema": "docsight.budget-conformance/v1",
        "fixtures": {
            "sample_tables.docx": "ae83cc0410fa35a70f9cac1c19d25e1cf59d6f05822ecf2de05d4b28982611ae",
            "sample_semantic.pdf": "a518b8cc3eb674dfc63dd83e6b85c7769d736203c7626960bc3dca69e06fea05"
        },
        "runs": [
            conformance_record("context-rich-24kb", &rich)?,
            conformance_record("context-balanced-4kb", &balanced)?,
            conformance_record("context-compact-3kb", &compact)?,
            conformance_record("peek-docx-compact-4kb", &peek)?,
            conformance_record("peek-pdf-compact-3kb", &pdf_peek)?
        ]
    });
    let golden_path = workspace().join("fixtures/conformance/m17-budget-projections.json");
    if std::env::var("DOCSIGHT_UPDATE_GOLDENS").is_ok_and(|value| value == "1") {
        let mut bytes = serde_json::to_vec_pretty(&actual)?;
        bytes.push(b'\n');
        std::fs::write(&golden_path, bytes)?;
    }
    let expected: Value = serde_json::from_slice(&std::fs::read(golden_path)?)?;
    assert_eq!(actual, expected);
    Ok(())
}
