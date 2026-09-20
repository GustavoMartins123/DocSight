#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::task::{RECEIPT_SCHEMA, RunOptions, SCENARIO_SCHEMA, run_with};
use xtask::tooling::common::{self, digest, json_bytes, workspace_root};
use xtask::tooling::process::{ProcessLimits, ProcessResult};

const TABLE_ID: &str = "tbl_6c22c8dd17adf7e32604b29241d9ab99";
const CELL_ID: &str = "cell_a1932a3e62fdc00c3e9528c5b008b5bc";

fn command_name(arguments: &[OsString]) -> Option<String> {
    let mut skip_binary = true;
    for argument in arguments {
        if skip_binary {
            skip_binary = false;
            continue;
        }
        let text = argument.to_string_lossy();
        if text.starts_with("--") {
            continue;
        }
        return Some(text.into_owned());
    }
    None
}

fn argument_after(arguments: &[OsString], flag: &str) -> Option<OsString> {
    let mut take = false;
    for argument in arguments {
        if take {
            return Some(argument.clone());
        }
        if argument == flag {
            take = true;
        }
    }
    None
}

fn fake_engine(
    arguments: &[OsString],
    _: &Path,
    _: &ProcessLimits,
    _: Option<&BTreeMap<OsString, OsString>>,
) -> common::Result<ProcessResult> {
    let command = command_name(arguments).ok_or_else(|| {
        common::ToolError::new("TEST_ARGUMENT", "Synthetic engine misses its command")
    })?;
    let document = argument_after(arguments, &command)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    if command == "inspect" && document.ends_with("unsupported.bin") {
        return Ok(outcome(
            10,
            Vec::new(),
            json_bytes(
                &json!({"schema": "docsight.agent/v2", "engine": "test", "error": {"code": "UNSUPPORTED_FORMAT", "exit_code": 10}}),
            )?,
        ));
    }
    let body = match command.as_str() {
        "inspect" => json!({"format": "docx", "tables": 1}),
        "coverage" => json!({"global": {"overall_fidelity": 0.5}, "pages": []}),
        "tables" => json!({"tables": [{"id": TABLE_ID}]}),
        "resolve" => {
            json!({"status": "resolved", "candidates": [{"object": {"id": TABLE_ID}, "matched_range": {}}]})
        }
        "table" => json!({"cells": [{"text": "Col 1"}]}),
        "text" => json!({"blocks": []}),
        "find" => json!({"matches": [{"object_id": CELL_ID, "matched": {"text": "EVENT"}}]}),
        "context" => json!({"context": {}, "status": "resolved"}),
        "evidence" => json!({"object_id": CELL_ID, "fidelity": {}}),
        "diff" => {
            json!({"summary": {"semantic_changes": 0}, "before_document": {"sha256": "a".repeat(64)}, "after_document": {"sha256": "a".repeat(64)}})
        }
        "render" => {
            let path = argument_after(arguments, "--out").ok_or_else(|| {
                common::ToolError::new("TEST_ARGUMENT", "Synthetic render misses its output")
            })?;
            let png = docsight_core::encode_png(1, 1, &[9, 9, 9])
                .map_err(|_| common::ToolError::new("TEST_PNG", "Cannot encode test PNG"))?;
            fs::write(Path::new(&path), &png)?;
            json!({"output_sha256": digest(&png), "output_bytes": png.len()})
        }
        _ => {
            return Err(common::ToolError::new(
                "TEST_ARGUMENT",
                "Synthetic engine rejects this command",
            ));
        }
    };
    agent(body)
}

fn engine_pair(root: &Path) -> TestResult<PathBuf> {
    let binary = root.join("docsight-engine");
    let worker = root.join(if cfg!(windows) {
        "docsight-worker.exe"
    } else {
        "docsight-worker"
    });
    fs::write(&binary, b"engine")?;
    fs::write(&worker, b"worker")?;
    Ok(binary)
}

fn run_scenario(name: &str) -> TestResult<xtask::task::TaskReceipt> {
    let fixture = Fixture::new()?;
    let engine = engine_pair(&fixture.root)?;
    let scenario = workspace_root().join("fixtures").join("tasks").join(name);
    let root = workspace_root().join("fixtures").join("validation");
    let mut runner = Callback(fake_engine);
    Ok(run_with(
        &RunOptions {
            scenario: &scenario,
            root: &root,
            engine: &engine,
        },
        &mut runner,
    )?)
}

fn deterministic_projection(receipt: &xtask::task::TaskReceipt) -> TestResult<Vec<u8>> {
    let steps: Vec<Value> = receipt
        .steps
        .iter()
        .map(|step| {
            json!({
                "id": step.id,
                "exit_code": step.exit_code,
                "stdout_sha256": step.stdout_sha256,
                "stderr_code": step.stderr_code,
            })
        })
        .collect();
    Ok(json_bytes(&json!({
        "scenario_id": receipt.scenario_id,
        "scenario_sha256": receipt.scenario_sha256,
        "engine_sha256": receipt.engine_sha256,
        "steps": steps,
        "budgets": {"invocations": receipt.budgets.invocations, "stdout_bytes": receipt.budgets.stdout_bytes},
    }))?)
}

#[test]
fn shipped_task_scenarios_complete_within_budgets() -> TestResult {
    for name in [
        "audit-docx-table.json",
        "extract-docx-text.json",
        "search-pdf-events.json",
        "compare-docx-pair.json",
        "diagnose-unsupported.json",
    ] {
        let receipt = run_scenario(name)?;
        assert!(receipt.passed);
        assert_eq!(receipt.schema, RECEIPT_SCHEMA);
        assert_eq!(receipt.scenario_sha256.len(), 64);
        assert_eq!(receipt.engine_sha256.len(), 64);
        assert!(!receipt.steps.is_empty());
        let again = run_scenario(name)?;
        assert_eq!(
            deterministic_projection(&receipt)?,
            deterministic_projection(&again)?,
            "receipt for {name}"
        );
    }
    assert_eq!(SCENARIO_SCHEMA, "docsight.task-scenario/v1");
    Ok(())
}

#[test]
fn task_run_rejects_bad_manifests_before_any_invocation() -> TestResult {
    let fixture = Fixture::new()?;
    let engine = engine_pair(&fixture.root)?;
    let root = workspace_root().join("fixtures").join("validation");
    let manifest = |id: &str, primary: &str, args: Value| {
        json!({
            "schema": "docsight.task-scenario/v1",
            "id": id,
            "title": id,
            "documents": {"primary": primary, "reference": null},
            "budgets": {"max_invocations": 4, "max_stdout_bytes": 65536},
            "steps": [{"id": "only", "args": args, "expect_exit": 0, "require": [], "values": {}}],
        })
    };
    for (id, primary, args) in [
        (
            "bad-command",
            "sample_tables.docx",
            json!(["vaporize", "sample_tables.docx"]),
        ),
        (
            "bad-schema",
            "sample_tables.docx",
            json!(["inspect", "sample_tables.docx"]),
        ),
        (
            "bad-traversal",
            "../outside.docx",
            json!(["inspect", "../outside.docx"]),
        ),
        (
            "bad-chain",
            "sample_tables.docx",
            json!(["inspect", "{missing./result/format}"]),
        ),
    ] {
        let mut scenario = manifest(id, primary, args);
        if id == "bad-schema" {
            scenario["schema"] = json!("docsight.something-else/v1");
        }
        let path = fixture.root.join(format!("{id}.json"));
        fs::write(&path, json_bytes(&scenario)?)?;
        let mut calls = 0u64;
        let mut runner = Callback(
            |arguments: &[OsString],
             cwd: &Path,
             limits: &ProcessLimits,
             environment: Option<&BTreeMap<OsString, OsString>>| {
                calls += 1;
                fake_engine(arguments, cwd, limits, environment)
            },
        );
        let outcome = run_with(
            &RunOptions {
                scenario: &path,
                root: &root,
                engine: &engine,
            },
            &mut runner,
        );
        assert!(outcome.is_err(), "{id}");
        assert_eq!(calls, 0, "{id}");
    }
    Ok(())
}

#[test]
fn task_run_enforces_the_invocation_budget() -> TestResult {
    let fixture = Fixture::new()?;
    let engine = engine_pair(&fixture.root)?;
    let root = workspace_root().join("fixtures").join("validation");
    let scenario = json!({
        "schema": "docsight.task-scenario/v1",
        "id": "over-budget",
        "title": "over-budget",
        "documents": {"primary": "sample_tables.docx", "reference": null},
        "budgets": {"max_invocations": 1, "max_stdout_bytes": 65536},
        "steps": [
            {"id": "first", "args": ["inspect", "{primary}"], "expect_exit": 0, "require": [], "values": {}},
            {"id": "second", "args": ["inspect", "{primary}"], "expect_exit": 0, "require": [], "values": {}},
        ],
    });
    let path = fixture.root.join("over-budget.json");
    fs::write(&path, json_bytes(&scenario)?)?;
    let mut runner = Callback(fake_engine);
    let outcome = run_with(
        &RunOptions {
            scenario: &path,
            root: &root,
            engine: &engine,
        },
        &mut runner,
    );
    assert!(outcome.is_err());
    Ok(())
}

#[test]
fn task_receipts_are_registered_tooling_evidence() -> TestResult {
    let registry: Value = serde_json::from_slice(&fs::read(
        workspace_root().join("schemas/tooling/v2/evidence.json"),
    )?)?;
    let variants = registry["oneOf"].as_array().ok_or("oneOf")?;
    for reference in ["#/$defs/task-scenario", "#/$defs/task-receipt"] {
        assert!(
            variants.iter().any(|variant| variant["$ref"] == reference),
            "{reference}"
        );
        assert!(
            registry
                .pointer(&format!("/$defs/{}", &reference["#/$defs/".len()..]))
                .is_some()
        );
    }
    Ok(())
}
