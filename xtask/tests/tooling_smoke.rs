#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::Duration;
use support::*;
use xtask::release::archive::verify_archive;
use xtask::release::{SMOKE_CHECKS, SmokeReceipt, TARGETS, native_target, validate_smoke_receipt};
use xtask::smoke::{smoke_archive_with, validate_png_bytes};
use xtask::tooling::common::{self, json_bytes, sha256_file};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Termination};

type Arguments = [OsString];

fn has(arguments: &Arguments, value: &str) -> bool {
    arguments.iter().any(|argument| argument == value)
}

fn names_file(arguments: &Arguments, suffix: &str) -> bool {
    arguments
        .iter()
        .any(|argument| argument.to_string_lossy().ends_with(suffix))
}

fn smoke_with<F>(mut adjust: F) -> TestResult<SmokeReceipt>
where
    F: FnMut(&Arguments, ProcessResult) -> common::Result<ProcessResult>,
{
    let fixture = Fixture::new()?;
    let archive = fixture.package(native_target()?, "dist")?;
    let mut runner = Callback(
        |arguments: &Arguments,
         cwd: &Path,
         limits: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>| {
            let result = synthetic_engine(arguments, cwd, limits, environment)?;
            adjust(arguments, result)
        },
    );
    Ok(smoke_archive_with(&archive, &mut runner)?)
}

fn failure<'a>(receipt: &'a SmokeReceipt, name: &str) -> Option<&'a str> {
    receipt
        .checks
        .iter()
        .find(|check| check.name == name)
        .and_then(|check| check.error_code.as_deref())
}

fn failed_checks(receipt: &SmokeReceipt) -> Vec<&str> {
    receipt
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.name.as_str())
        .collect()
}

#[test]
fn passing_engine_produces_a_complete_candidate_bound_receipt() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(native_target()?, "dist")?;
    let mut observed = Vec::new();
    let mut runner = Callback(
        |arguments: &Arguments,
         cwd: &Path,
         limits: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>| {
            observed.push((arguments.to_vec(), cwd.to_path_buf(), limits.clone()));
            synthetic_engine(arguments, cwd, limits, environment)
        },
    );
    let receipt = smoke_archive_with(&archive, &mut runner)?;
    assert!(receipt.passed, "{:?}", failed_checks(&receipt));
    let names: Vec<_> = receipt
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    assert_eq!(names, SMOKE_CHECKS);
    assert_eq!(receipt.archive_sha256, sha256_file(&archive)?);
    assert_eq!(receipt.revision, REVISION);
    let manifest = verify_archive(&archive)?;
    validate_smoke_receipt(&receipt, &manifest, &receipt.archive_sha256)?;
    assert_eq!(observed.len(), SMOKE_CHECKS.len());
    for (arguments, cwd, limits) in &observed {
        let executable = Path::new(&arguments[0]);
        assert!(!executable.starts_with(&fixture.root));
        assert!(
            executable
                .parent()
                .is_some_and(|parent| parent.ends_with("package"))
        );
        assert!(cwd.ends_with("work"));
        assert_eq!(limits.timeout, Duration::from_secs(45));
    }
    assert!(
        observed
            .iter()
            .any(|(arguments, _, _)| has(arguments, "--sandbox") && has(arguments, "inspect"))
    );
    Ok(())
}

#[test]
fn stderr_on_a_successful_agent_call_fails_that_check_only() -> TestResult {
    let receipt = smoke_with(|arguments, mut result| {
        if has(arguments, "inspect")
            && !has(arguments, "--sandbox")
            && names_file(arguments, ".pdf")
        {
            result.stderr = b"progress".to_vec();
        }
        Ok(result)
    })?;
    assert!(!receipt.passed);
    assert_eq!(receipt.checks.len(), SMOKE_CHECKS.len());
    assert_eq!(
        failure(&receipt, "pdf_inspect"),
        Some("SMOKE_STDERR_CONTAMINATION")
    );
    assert!(failed_checks(&receipt).contains(&"pdf_inspect"));
    assert_eq!(failure(&receipt, "docx_inspect"), None);
    Ok(())
}

#[test]
fn repeated_inspection_must_produce_identical_bytes() -> TestResult {
    let mut calls = 0;
    let receipt = smoke_with(|arguments, result| {
        if has(arguments, "inspect")
            && !has(arguments, "--sandbox")
            && names_file(arguments, ".docx")
        {
            calls += 1;
            if calls == 2 {
                return agent(json!({"format": "docx", "pages": 2}));
            }
        }
        Ok(result)
    })?;
    assert_eq!(failed_checks(&receipt), vec!["docx_determinism"]);
    assert_eq!(
        failure(&receipt, "docx_determinism"),
        Some("SMOKE_NONDETERMINISTIC")
    );
    Ok(())
}

#[test]
fn version_capabilities_and_diff_contracts_are_enforced() -> TestResult {
    let receipt = smoke_with(|arguments, result| {
        if has(arguments, "--version") && arguments.len() == 2 && !has(arguments, "--agent") {
            return Ok(outcome(0, b"docsight 0.0.0\n".to_vec(), Vec::new()));
        }
        if has(arguments, "capabilities") {
            return agent(json!({"commands": [{"name": "inspect"}]}));
        }
        if has(arguments, "diff") {
            return agent(json!({"summary": {"semantic_changes": 1}}));
        }
        Ok(result)
    })?;
    assert_eq!(failure(&receipt, "version"), Some("SMOKE_VERSION_MISMATCH"));
    assert_eq!(
        failure(&receipt, "capabilities"),
        Some("SMOKE_CAPABILITIES")
    );
    assert_eq!(
        failure(&receipt, "identical_diff"),
        Some("SMOKE_IDENTICAL_DIFF")
    );
    assert!(!receipt.passed);
    Ok(())
}

#[test]
fn typed_errors_must_keep_stdout_clean_and_use_their_exit_code() -> TestResult {
    let receipt = smoke_with(|arguments, result| {
        if has(arguments, "--json-errors") {
            return Ok(outcome(
                10,
                b"partial".to_vec(),
                json_bytes(&json!({"code": "UNSUPPORTED_FORMAT"}))?,
            ));
        }
        Ok(result)
    })?;
    assert_eq!(
        failure(&receipt, "typed_error"),
        Some("SMOKE_ERROR_CONTRACT")
    );

    let receipt = smoke_with(|arguments, result| {
        if has(arguments, "--json-errors") {
            return Ok(outcome(0, Vec::new(), Vec::new()));
        }
        Ok(result)
    })?;
    assert_eq!(failure(&receipt, "typed_error"), Some("SMOKE_EXIT_CODE"));
    Ok(())
}

#[test]
fn process_limits_runner_failures_and_invalid_renders_are_recorded() -> TestResult {
    let receipt = smoke_with(|arguments, mut result| {
        if has(arguments, "completions") && has(arguments, "zsh") {
            result.termination = Some(Termination::Timeout);
        }
        if has(arguments, "completions") && has(arguments, "fish") {
            return Err(common::ToolError::new(
                "EXECUTABLE_UNAVAILABLE",
                "Required executable is unavailable",
            ));
        }
        if has(arguments, "render") && names_file(arguments, "pdf.png") {
            let index = arguments
                .iter()
                .position(|argument| argument == "--out")
                .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output"))?;
            fs::write(&arguments[index + 1], b"\x89PNG\r\n\x1a\ntruncated")?;
        }
        Ok(result)
    })?;
    assert_eq!(
        failure(&receipt, "completion_zsh"),
        Some("SMOKE_PROCESS_LIMIT")
    );
    assert_eq!(
        failure(&receipt, "completion_fish"),
        Some("EXECUTABLE_UNAVAILABLE")
    );
    assert_eq!(failure(&receipt, "pdf_render"), Some("SMOKE_INVALID_PNG"));
    assert_eq!(failure(&receipt, "docx_render"), None);
    assert_eq!(receipt.checks.len(), SMOKE_CHECKS.len());
    assert!(!receipt.passed);
    Ok(())
}

#[test]
fn archives_for_another_platform_are_not_executed() -> TestResult {
    let fixture = Fixture::new()?;
    let native = native_target()?;
    let foreign = TARGETS
        .into_iter()
        .find(|target| *target != native)
        .ok_or("foreign target")?;
    let archive = fixture.package(foreign, "dist")?;
    let mut calls = 0;
    let mut runner = Callback(
        |_: &Arguments, _: &Path, _: &ProcessLimits, _: Option<&BTreeMap<OsString, OsString>>| {
            calls += 1;
            Ok(version())
        },
    );
    assert_eq!(
        code(smoke_archive_with(&archive, &mut runner)),
        Some("SMOKE_HOST_MISMATCH")
    );
    assert_eq!(calls, 0);
    Ok(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], content: &[u8]) -> TestResult<Vec<u8>> {
    let mut bytes = u32::try_from(content.len())?.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(content);
    let mut covered = kind.to_vec();
    covered.extend_from_slice(content);
    bytes.extend_from_slice(&crc32(&covered).to_be_bytes());
    Ok(bytes)
}

#[test]
fn png_validation_checks_structure_checksums_and_dimensions() -> TestResult {
    let valid = small_png()?;
    validate_png_bytes(&valid)?;

    assert_eq!(
        code(validate_png_bytes(&valid[..valid.len() - 1])),
        Some("SMOKE_INVALID_PNG")
    );
    let mut trailing = valid.clone();
    trailing.push(0);
    assert_eq!(
        code(validate_png_bytes(&trailing)),
        Some("SMOKE_INVALID_PNG")
    );
    let mut corrupted = valid.clone();
    let middle = valid.len() / 2;
    corrupted[middle] ^= 0xff;
    assert_eq!(
        code(validate_png_bytes(&corrupted)),
        Some("SMOKE_INVALID_PNG")
    );
    assert_eq!(
        code(validate_png_bytes(b"GIF89a")),
        Some("SMOKE_INVALID_PNG")
    );

    let mut header = Vec::new();
    header.extend_from_slice(&20_000u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut oversized = b"\x89PNG\r\n\x1a\n".to_vec();
    oversized.extend(chunk(b"IHDR", &header)?);
    oversized.extend(chunk(b"IDAT", &[0])?);
    oversized.extend(chunk(b"IEND", &[])?);
    assert_eq!(
        code(validate_png_bytes(&oversized)),
        Some("SMOKE_INVALID_PNG")
    );
    Ok(())
}
