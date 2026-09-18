#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::release::archive::{Manifest, verify_archive};
use xtask::release::lifecycle::{
    LIFECYCLE_CHECKS, LifecycleReceipt, compare_capabilities, declares_breaking_change, is_update,
    verify_lifecycle_with,
};
use xtask::release::native_target;
use xtask::tooling::common::{self, ToolError, workspace_version};
use xtask::tooling::process::{ProcessLimits, ProcessResult};

fn capabilities(commands: &[&str], not_found_exit: i64) -> Value {
    json!({
        "protocol": "docsight.agent/v2",
        "coordinate_system": "points at 1/72 inch with page origin at the top-left",
        "document_formats": ["docx", "pdf"],
        "errors": {"codes": [
            {"code": "UNSUPPORTED_FORMAT", "exit_code": 10},
            {"code": "OBJECT_NOT_FOUND", "exit_code": not_found_exit}
        ]},
        "commands": commands.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
    })
}

#[derive(Clone)]
struct Engine {
    previous: Value,
    candidate: Value,
    candidate_version: Option<String>,
    rollback_differs: bool,
    writes_home: bool,
}

impl Engine {
    fn compatible() -> Self {
        let full = capabilities(&["inspect", "render", "find", "capabilities"], 21);
        Self {
            previous: full.clone(),
            candidate: full,
            candidate_version: None,
            rollback_differs: false,
            writes_home: false,
        }
    }
}

fn verify(engine: Engine, previous: &Path, candidate: &Path) -> common::Result<LifecycleReceipt> {
    let previous_version = verify_archive(previous)?.version;
    let inspections = Cell::new(0u32);
    let mut runner = Callback(
        |arguments: &[OsString],
         _: &Path,
         _: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>|
         -> common::Result<ProcessResult> {
            let environment = environment
                .ok_or_else(|| ToolError::new("TEST_ENV", "isolated environment required"))?;
            let home = environment
                .get(&OsString::from("HOME"))
                .ok_or_else(|| ToolError::new("TEST_ENV", "HOME must be isolated"))?;
            let binary = PathBuf::from(&arguments[0]);
            let version = installed_version(&binary);
            let is_candidate = version != previous_version;
            let contains = |value: &str| arguments.iter().any(|argument| argument == value);
            if contains("--version") {
                let reported = match (&engine.candidate_version, is_candidate) {
                    (Some(wrong), true) => wrong.clone(),
                    _ => version,
                };
                return Ok(outcome(
                    0,
                    format!("docsight {reported}\n").into_bytes(),
                    Vec::new(),
                ));
            }
            if contains("capabilities") {
                return agent(if is_candidate {
                    engine.candidate.clone()
                } else {
                    engine.previous.clone()
                });
            }
            if engine.writes_home {
                fs::write(Path::new(home).join(".docsight-state"), b"state")?;
            }
            let format = if arguments
                .last()
                .is_some_and(|argument| argument.to_string_lossy().ends_with(".pdf"))
            {
                "pdf"
            } else {
                "docx"
            };
            inspections.set(inspections.get() + 1);
            let pages = if engine.rollback_differs && !is_candidate && inspections.get() > 4 {
                2
            } else {
                1
            };
            agent(json!({"format": format, "pages": pages}))
        },
    );
    verify_lifecycle_with(previous, candidate, native_target()?, &mut runner)
}

fn packages(fixture: &Fixture, candidate_version: &str) -> TestResult<(PathBuf, PathBuf)> {
    let previous = fixture.package(native_target()?, "dist")?;
    let candidate = relabel(&previous, candidate_version)?;
    Ok((previous, candidate))
}

fn failed_at(receipt: &LifecycleReceipt) -> Option<(&str, Option<&str>)> {
    receipt
        .checks
        .iter()
        .find(|check| !check.passed)
        .map(|check| (check.name.as_str(), check.error_code.as_deref()))
}

#[test]
fn a_compatible_update_installs_side_by_side_and_rolls_back() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.1.5")?;
    let receipt = verify(Engine::compatible(), &previous, &candidate)?;
    assert!(receipt.passed, "{:?}", failed_at(&receipt));
    assert_eq!(
        receipt
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>(),
        LIFECYCLE_CHECKS
    );
    assert_eq!(receipt.previous.version, workspace_version());
    assert_eq!(receipt.candidate.version, "0.1.5");
    let compatibility = receipt.compatibility.ok_or("compatibility")?;
    assert!(!compatibility.breaking);
    assert!(!compatibility.breaking_change_declared);
    Ok(())
}

#[test]
fn an_undeclared_breaking_change_fails_the_update() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.1.5")?;
    let mut engine = Engine::compatible();
    engine.candidate = capabilities(&["inspect", "render", "capabilities"], 22);
    let receipt = verify(engine, &previous, &candidate)?;
    assert!(!receipt.passed);
    assert_eq!(
        failed_at(&receipt),
        Some(("compatibility", Some("UNDECLARED_BREAKING_CHANGE")))
    );
    assert_eq!(receipt.checks.len(), 7);
    let compatibility = receipt.compatibility.ok_or("compatibility")?;
    assert!(compatibility.breaking);
    assert_eq!(compatibility.removed_commands, ["find"]);
    assert_eq!(compatibility.changed_error_codes, ["OBJECT_NOT_FOUND"]);
    Ok(())
}

#[test]
fn a_breaking_change_is_accepted_when_the_version_declares_it() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.2.0")?;
    let mut engine = Engine::compatible();
    engine.candidate = capabilities(&["inspect", "render", "capabilities"], 21);
    let receipt = verify(engine, &previous, &candidate)?;
    assert!(receipt.passed, "{:?}", failed_at(&receipt));
    let compatibility = receipt.compatibility.ok_or("compatibility")?;
    assert!(compatibility.breaking && compatibility.breaking_change_declared);
    Ok(())
}

#[test]
fn lifecycle_failures_stop_at_the_step_that_observed_them() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.1.5")?;
    let mut wrong_version = Engine::compatible();
    wrong_version.candidate_version = Some(workspace_version().into());
    let receipt = verify(wrong_version, &previous, &candidate)?;
    assert_eq!(
        failed_at(&receipt),
        Some(("candidate_runs", Some("LIFECYCLE_VERSION_MISMATCH")))
    );

    let mut rollback = Engine::compatible();
    rollback.rollback_differs = true;
    let receipt = verify(rollback, &previous, &candidate)?;
    assert_eq!(
        failed_at(&receipt),
        Some(("rollback", Some("LIFECYCLE_ROLLBACK_CHANGED")))
    );

    let mut state = Engine::compatible();
    state.writes_home = true;
    let receipt = verify(state, &previous, &candidate)?;
    assert_eq!(
        failed_at(&receipt),
        Some(("no_external_state", Some("LIFECYCLE_EXTERNAL_STATE")))
    );
    assert!(!receipt.passed);
    Ok(())
}

#[test]
fn tampered_or_unverifiable_packages_are_not_installed() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.1.5")?;
    let sidecar = candidate.with_file_name(format!(
        "{}.sha256",
        candidate.file_name().ok_or("name")?.to_string_lossy()
    ));
    fs::write(&sidecar, format!("{}  wrong.zip\n", "0".repeat(64)))?;
    let receipt = verify(Engine::compatible(), &previous, &candidate)?;
    assert_eq!(
        failed_at(&receipt),
        Some((
            "install_candidate_side_by_side",
            Some("ARCHIVE_CHECKSUM_MISMATCH")
        ))
    );
    Ok(())
}

#[test]
fn only_newer_packages_for_the_native_host_are_updates() -> TestResult {
    let fixture = Fixture::new()?;
    let (previous, candidate) = packages(&fixture, "0.1.5")?;
    assert_eq!(
        code(verify(Engine::compatible(), &candidate, &previous)),
        Some("NOT_AN_UPDATE")
    );
    assert_eq!(
        code(verify(Engine::compatible(), &previous, &previous)),
        Some("NOT_AN_UPDATE")
    );
    let foreign = fixture.package("aarch64-apple-darwin", "foreign")?;
    let foreign_candidate = relabel(&foreign, "0.1.5")?;
    assert_eq!(
        code(verify(Engine::compatible(), &foreign, &foreign_candidate)),
        Some("LIFECYCLE_HOST_MISMATCH")
    );
    Ok(())
}

#[test]
fn version_order_and_declared_breaking_changes_follow_semantic_versioning() -> TestResult {
    for (previous, candidate, update) in [
        ("0.1.4", "0.1.5", true),
        ("0.1.4", "0.2.0", true),
        ("1.9.9", "2.0.0", true),
        ("1.0.0-beta.1", "1.0.0", true),
        ("1.0.0", "1.0.0-beta.1", false),
        ("1.0.0", "1.0.0", false),
        ("0.2.0", "0.1.9", false),
    ] {
        assert_eq!(
            is_update(previous, candidate)?,
            update,
            "{previous} {candidate}"
        );
    }
    for (previous, candidate, declared) in [
        ("0.1.4", "0.1.5", false),
        ("0.1.4", "0.2.0", true),
        ("0.9.0", "1.0.0", true),
        ("1.2.0", "1.3.0", false),
        ("1.2.0", "2.0.0", true),
    ] {
        assert_eq!(
            declares_breaking_change(previous, candidate)?,
            declared,
            "{previous} {candidate}"
        );
    }
    Ok(())
}

#[test]
fn removed_schemas_and_protocol_changes_are_breaking() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(native_target()?, "dist")?;
    let previous: Manifest = verify_archive(&archive)?;
    let mut candidate = previous.clone();
    candidate.version = "0.1.5".into();
    candidate
        .files
        .retain(|record| record.path != "schemas/ir/v1/document-ir.json");
    let full = capabilities(&["inspect"], 21);
    let compatibility = compare_capabilities(&full, &full, &previous, &candidate)?;
    assert!(compatibility.breaking);
    assert_eq!(
        compatibility.removed_schemas,
        ["schemas/ir/v1/document-ir.json"]
    );

    let mut next_protocol = full.clone();
    next_protocol["protocol"] = json!("docsight.agent/v3");
    let compatibility = compare_capabilities(&full, &next_protocol, &previous, &previous)?;
    assert!(compatibility.protocol_changed && compatibility.breaking);

    let mut incomplete = full.clone();
    incomplete["errors"] = json!({});
    assert_eq!(
        code(compare_capabilities(
            &full,
            &incomplete,
            &previous,
            &previous
        )),
        Some("LIFECYCLE_CAPABILITIES")
    );
    Ok(())
}
