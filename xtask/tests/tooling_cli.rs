#[allow(dead_code)]
mod support;
use clap::{CommandFactory, Parser};
use std::process::Command;
use support::*;
use xtask::cli::Cli;
use xtask::tooling::common::*;

#[test]
fn maintenance_cli_definition_is_internally_consistent() {
    Cli::command().debug_assert();
}
#[test]
fn help_exposes_every_native_maintenance_command() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_maint"))
        .arg("--help")
        .output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    for name in [
        "release",
        "notices",
        "changelog",
        "smoke",
        "corpus",
        "beta",
        "validate",
        "readiness",
        "rust-only",
    ] {
        assert!(text.contains(name));
    }
    Ok(())
}
#[test]
fn package_requires_both_native_binaries() {
    assert!(
        Cli::try_parse_from([
            "xtask",
            "release",
            "package",
            "--binary",
            "a",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--revision",
            REVISION,
            "--notices",
            "notices",
            "--out",
            "dist"
        ])
        .is_err()
    );
}
#[test]
fn beta_requires_explicit_consent_in_the_public_cli() {
    assert!(
        Cli::try_parse_from([
            "xtask",
            "beta",
            "collect",
            "--archive",
            "a.zip",
            "--participant",
            "beta-001",
            "--operation",
            "inspect",
            "--experience",
            "clear",
            "--document",
            "private.docx",
            "--out",
            "observation.json"
        ])
        .is_err()
    );
}
#[test]
fn inventory_command_runs_without_the_document_engine_or_an_interpreter() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_maint"))
        .args(["corpus", "validate"])
        .arg("--root")
        .arg(workspace_root())
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_json(&output.stdout)?;
    assert_eq!(report["executed"], false);
    assert_eq!(report["schema"], "docsight.corpus-inventory/v2");
    assert_eq!(report["cases"], 17);
    assert_eq!(report["classes"]["malformed-container"], 3);
    assert!(
        report["declared_classes"]
            .as_array()
            .is_some_and(|classes| classes.len() >= 10)
    );
    Ok(())
}
#[test]
fn wrong_release_tags_fail_with_a_typed_error() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_maint"))
        .args(["release", "version", "--tag", "v9.9.9"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(parse_json(&output.stderr)?["code"], "TAG_VERSION_MISMATCH");
    Ok(())
}
#[test]
fn readiness_reports_missing_evidence_instead_of_manufacturing_approval() -> TestResult {
    let fixture = Fixture::new()?;
    let output = Command::new(env!("CARGO_BIN_EXE_maint"))
        .args(["readiness", "--revision", REVISION, "--evidence"])
        .arg(fixture.root.join("absent"))
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let report = parse_json(&output.stdout)?;
    assert_eq!(report["ready_for_v1"], false);
    assert_eq!(report["criteria"].as_array().ok_or("criteria")?.len(), 7);
    Ok(())
}
#[test]
fn configuration_emits_valid_literal_matrix_without_inline_scripting() -> TestResult {
    let fixture = Fixture::new()?;
    let file = fixture.root.join("github-output");
    std::fs::write(&file, b"")?;
    let output = Command::new(env!("CARGO_BIN_EXE_maint"))
        .args(["release", "configuration", "--github-output"])
        .arg(&file)
        .output()?;
    assert!(output.status.success());
    let report = parse_json(&output.stdout)?;
    assert_eq!(
        report["matrix"]["include"]
            .as_array()
            .ok_or("matrix")?
            .len(),
        5
    );
    let text = std::fs::read_to_string(file)?;
    assert_eq!(text.lines().count(), 2);
    assert!(text.starts_with("matrix={"));
    Ok(())
}
