#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::beta::{
    CollectOptions, Report, aggregate, collect_with, known_diagnostics, validate_report,
};
use xtask::release::{TARGETS, native_target};
use xtask::tooling::common::{self, digest, json_bytes, workspace_version};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Termination};

type Arguments = [OsString];
type Engine = Box<dyn Fn() -> common::Result<ProcessResult>>;

fn has(arguments: &Arguments, value: &str) -> bool {
    arguments.iter().any(|argument| argument == value)
}

fn options(operation: &str, document: Option<PathBuf>) -> CollectOptions {
    CollectOptions {
        participant: "beta-001".into(),
        operation: operation.into(),
        experience: "clear".into(),
        consent: true,
        document,
        reference: None,
        password_file: None,
        include_document_digest: false,
    }
}

struct Session {
    fixture: Fixture,
    archive: PathBuf,
    document: PathBuf,
}

impl Session {
    fn new() -> TestResult<Self> {
        let fixture = Fixture::new()?;
        let archive = fixture.package(native_target()?, "dist")?;
        let private = fixture.root.join("private");
        fs::create_dir(&private)?;
        let document = private.join("Confidential Salary Plan.docx");
        fs::write(&document, b"confidential salary figures")?;
        Ok(Self {
            fixture,
            archive,
            document,
        })
    }

    fn collect<F>(&self, options: &CollectOptions, mut engine: F) -> common::Result<Report>
    where
        F: FnMut(&Arguments) -> common::Result<ProcessResult>,
    {
        let mut runner = Callback(
            |arguments: &Arguments,
             _: &Path,
             _: &ProcessLimits,
             environment: Option<&BTreeMap<OsString, OsString>>| {
                common::require(
                    environment.is_some(),
                    "TEST_ENV",
                    "Beta collection must isolate the environment",
                )?;
                if has(arguments, "--version") {
                    return Ok(version());
                }
                engine(arguments)
            },
        );
        collect_with(&self.archive, options, &mut runner)
    }
}

fn success_with_warnings(codes: &[&str]) -> common::Result<ProcessResult> {
    let warnings: Vec<_> = codes.iter().map(|code| json!({"code": code})).collect();
    Ok(outcome(
        0,
        json_bytes(&json!({
            "schema": "docsight.agent/v2",
            "result": {"format": "docx"},
            "warnings": warnings,
        }))?,
        Vec::new(),
    ))
}

fn typed_error(code: &str, exit: i64) -> common::Result<ProcessResult> {
    Ok(outcome(
        exit,
        Vec::new(),
        json_bytes(&json!({
            "schema": "docsight.agent/v2",
            "error": {"code": code, "exit_code": exit},
        }))?,
    ))
}

#[test]
fn invalid_requests_fail_before_any_archive_or_process_access() {
    let absent = PathBuf::from("absent-archive.zip");
    let document = Some(PathBuf::from("absent.docx"));
    let mut variants = Vec::new();

    let mut request = options("inspect", document.clone());
    request.consent = false;
    variants.push((request, "BETA_CONSENT_REQUIRED"));

    let mut request = options("inspect", document.clone());
    request.participant = "alice".into();
    variants.push((request, "INVALID_PARTICIPANT"));

    variants.push((options("edit", document.clone()), "INVALID_BETA_OPERATION"));
    variants.push((options("inspect", None), "BETA_DOCUMENT_REQUIRED"));
    variants.push((options("diff", document.clone()), "BETA_REFERENCE_REQUIRED"));

    let mut request = options("inspect", document.clone());
    request.reference = Some(PathBuf::from("reference.docx"));
    variants.push((request, "BETA_REFERENCE_REQUIRED"));

    variants.push((options("capabilities", document), "UNUSED_BETA_INPUT"));

    for (request, expected) in variants {
        let mut calls = 0;
        let mut runner = Callback(
            |_: &Arguments,
             _: &Path,
             _: &ProcessLimits,
             _: Option<&BTreeMap<OsString, OsString>>| {
                calls += 1;
                Ok(version())
            },
        );
        assert_eq!(
            code(collect_with(&absent, &request, &mut runner)),
            Some(expected)
        );
        assert_eq!(calls, 0);
    }
}

#[test]
fn observations_exclude_document_names_contents_and_paths() -> TestResult {
    let session = Session::new()?;
    let mut invocations = Vec::new();
    let report = session.collect(
        &options("inspect", Some(session.document.clone())),
        |arguments| {
            invocations.push(arguments.to_vec());
            success_with_warnings(&["UNSUPPORTED_FORMAT", "PRIVATE_UNDOCUMENTED_CODE"])
        },
    )?;
    assert_eq!(report.outcome, "success");
    assert_eq!(report.diagnostic_codes, vec!["UNSUPPORTED_FORMAT"]);
    assert_eq!(report.unknown_diagnostic_count, 1);
    assert_eq!(report.document_sha256, None);
    assert_eq!(report.document_size_bytes, 27);
    assert_eq!(report.version, workspace_version());
    let serialized = String::from_utf8(json_bytes(&report)?)?;
    for private in [
        "Confidential",
        "Salary",
        "salary figures",
        "PRIVATE_UNDOCUMENTED_CODE",
    ] {
        assert!(!serialized.contains(private), "{private}");
    }
    assert!(!serialized.contains(session.fixture.root.to_string_lossy().as_ref()));
    let arguments = invocations.first().ok_or("engine was not invoked")?;
    for flag in ["--agent", "--sandbox", "--max-bytes", "inspect"] {
        assert!(has(arguments, flag), "{flag}");
    }
    Ok(())
}

#[test]
fn document_digest_requires_a_separate_opt_in() -> TestResult {
    let session = Session::new()?;
    let mut request = options("inspect", Some(session.document.clone()));
    request.include_document_digest = true;
    let report = session.collect(&request, |_| success_with_warnings(&[]))?;
    assert_eq!(
        report.document_sha256,
        Some(digest(b"confidential salary figures"))
    );
    Ok(())
}

#[test]
fn engine_outcomes_are_classified_without_trusting_labels() -> TestResult {
    let session = Session::new()?;
    let request = options("inspect", Some(session.document.clone()));
    let cases: Vec<(&str, Engine)> = vec![
        ("error", Box::new(|| typed_error("UNSUPPORTED_FORMAT", 10))),
        (
            "crash",
            Box::new(|| Ok(outcome(-11, Vec::new(), Vec::new()))),
        ),
        (
            "timeout",
            Box::new(|| {
                let mut result = outcome(0, Vec::new(), Vec::new());
                result.termination = Some(Termination::Timeout);
                Ok(result)
            }),
        ),
        (
            "output_limit",
            Box::new(|| {
                let mut result = outcome(0, Vec::new(), Vec::new());
                result.termination = Some(Termination::OutputLimit);
                Ok(result)
            }),
        ),
        (
            "invalid_protocol",
            Box::new(|| Ok(outcome(0, b"plain text".to_vec(), Vec::new()))),
        ),
        (
            "invalid_protocol",
            Box::new(|| {
                let mut result = typed_error("UNSUPPORTED_FORMAT", 10)?;
                result.returncode = 11;
                Ok(result)
            }),
        ),
    ];
    for (expected, engine) in cases {
        let report = session.collect(&request, |_| engine())?;
        assert_eq!(report.outcome, expected);
    }
    let report = session.collect(&request, |_| typed_error("UNSUPPORTED_FORMAT", 10))?;
    assert_eq!(report.exit_code, 10);
    assert_eq!(report.diagnostic_codes, vec!["UNSUPPORTED_FORMAT"]);
    Ok(())
}

#[test]
fn passwords_travel_only_as_a_bounded_file_reference() -> TestResult {
    let session = Session::new()?;
    let password = session.fixture.root.join("private/password.txt");
    fs::write(&password, b"hunter2-secret\n")?;
    let mut request = options("inspect", Some(session.document.clone()));
    request.password_file = Some(password.clone());
    let mut invocations = Vec::new();
    let report = session.collect(&request, |arguments| {
        invocations.push(arguments.to_vec());
        success_with_warnings(&[])
    })?;
    let arguments = invocations.first().ok_or("engine was not invoked")?;
    let position = arguments
        .iter()
        .position(|argument| argument == "--password-file")
        .ok_or("password flag")?;
    assert_eq!(
        PathBuf::from(&arguments[position + 1]),
        password.canonicalize()?
    );
    assert!(
        arguments
            .iter()
            .all(|argument| !argument.to_string_lossy().contains("hunter2"))
    );
    assert!(!String::from_utf8(json_bytes(&report)?)?.contains("hunter2"));

    fs::write(&password, vec![b'x'; 132])?;
    let mut calls = 0;
    let oversized = session.collect(&request, |_| {
        calls += 1;
        success_with_warnings(&[])
    });
    assert_eq!(code(oversized), Some("FILE_SIZE_LIMIT"));
    assert_eq!(calls, 0);
    Ok(())
}

#[test]
fn inputs_candidate_identity_and_platform_are_verified() -> TestResult {
    let session = Session::new()?;
    let request = options("inspect", Some(session.document.clone()));
    let document = session.document.clone();
    let changed = session.collect(&request, |_| {
        fs::write(&document, b"replaced during collection")?;
        success_with_warnings(&[])
    });
    assert_eq!(code(changed), Some("BETA_INPUT_CHANGED"));

    let session = Session::new()?;
    let request = options("inspect", Some(session.document.clone()));
    let mut runner = Callback(
        |_: &Arguments, _: &Path, _: &ProcessLimits, _: Option<&BTreeMap<OsString, OsString>>| {
            Ok(outcome(0, b"docsight 0.0.0\n".to_vec(), Vec::new()))
        },
    );
    assert_eq!(
        code(collect_with(&session.archive, &request, &mut runner)),
        Some("BETA_VERSION_MISMATCH")
    );

    let fixture = Fixture::new()?;
    let native = native_target()?;
    let foreign = TARGETS
        .into_iter()
        .find(|target| *target != native)
        .ok_or("foreign target")?;
    let archive = fixture.package(foreign, "dist")?;
    let mut runner = Callback(
        |_: &Arguments, _: &Path, _: &ProcessLimits, _: Option<&BTreeMap<OsString, OsString>>| {
            Ok(version())
        },
    );
    assert_eq!(
        code(collect_with(
            &archive,
            &options("capabilities", None),
            &mut runner
        )),
        Some("BETA_HOST_MISMATCH")
    );
    Ok(())
}

fn report(participant: &str, operation: &str, elapsed_ms: u64) -> Report {
    Report {
        schema: "docsight.beta-report/v1".into(),
        version: workspace_version().into(),
        revision: REVISION.into(),
        target: "x86_64-unknown-linux-gnu".into(),
        archive_sha256: "b".repeat(64),
        participant: participant.into(),
        operation: operation.into(),
        experience: "clear".into(),
        outcome: "success".into(),
        exit_code: 0,
        elapsed_ms,
        stdout_bytes: 10,
        stderr_bytes: 0,
        document_size_bytes: 100,
        document_sha256: None,
        diagnostic_codes: Vec::new(),
        unknown_diagnostic_count: 0,
    }
}

#[test]
fn stored_reports_must_be_internally_consistent() -> TestResult {
    let fixture = Fixture::new()?;
    let allowed = known_diagnostics(&fixture.root)?;
    assert!(allowed.contains("UNSUPPORTED_FORMAT"));
    assert!(validate_report(report("beta-001", "inspect", 5), &allowed).is_ok());

    let mut variants: Vec<(Report, &str)> = Vec::new();
    let mut value = report("beta-001", "inspect", 5);
    value.schema = "docsight.beta-report/v2".into();
    variants.push((value, "INVALID_BETA_SCHEMA"));
    variants.push((report("beta-01", "inspect", 5), "INVALID_PARTICIPANT"));
    variants.push((report("beta-001", "edit", 5), "INVALID_BETA_VALUE"));
    variants.push((report("beta-001", "inspect", 3_600_001), "INVALID_INTEGER"));

    for (outcome, exit_code) in [
        ("crash", 0),
        ("success", 1),
        ("error", 0),
        ("invalid_protocol", -11),
    ] {
        let mut value = report("beta-001", "inspect", 5);
        value.outcome = outcome.into();
        value.exit_code = exit_code;
        variants.push((value, "INCONSISTENT_BETA_OUTCOME"));
    }

    let mut value = report("beta-001", "inspect", 5);
    value.diagnostic_codes = vec!["UNDOCUMENTED_CODE".into()];
    variants.push((value, "INVALID_DIAGNOSTIC_CODES"));
    let mut value = report("beta-001", "inspect", 5);
    value.diagnostic_codes = vec!["UNSUPPORTED_FORMAT".into(), "UNSUPPORTED_FORMAT".into()];
    variants.push((value, "INVALID_DIAGNOSTIC_CODES"));

    for (value, expected) in variants {
        assert_eq!(code(validate_report(value, &allowed)), Some(expected));
    }
    Ok(())
}

#[test]
fn aggregation_counts_distinct_observations_and_latency() -> TestResult {
    let fixture = Fixture::new()?;
    let directory = fixture.root.join("beta");
    save(
        &directory.join("one.json"),
        &report("beta-001", "inspect", 10),
    )?;
    save(
        &directory.join("two.json"),
        &report("beta-002", "inspect", 30),
    )?;
    save(
        &directory.join("three.json"),
        &report("beta-002", "render", 7),
    )?;
    let summary = aggregate(&directory, &fixture.root)?;
    assert_eq!(summary["reports"], 3);
    let participants: BTreeSet<_> = summary["participants"]
        .as_array()
        .ok_or("participants")?
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert_eq!(participants, ["beta-001", "beta-002"].into());
    assert_eq!(summary["operations"]["inspect"], 2);
    assert_eq!(summary["performance"]["inspect"]["p50_ms"], 20.0);
    assert_eq!(summary["performance"]["inspect"]["maximum_ms"], 30);
    assert_eq!(summary["performance"]["render"]["p50_ms"], 7);
    assert_eq!(
        summary["report_sha256s"].as_array().ok_or("hashes")?.len(),
        3
    );
    Ok(())
}

#[test]
fn copied_mixed_and_absent_observations_are_rejected() -> TestResult {
    let fixture = Fixture::new()?;
    let directory = fixture.root.join("copied");
    let original = report("beta-001", "inspect", 10);
    save(&directory.join("original.json"), &original)?;
    fs::write(
        directory.join("reformatted.json"),
        serde_json::to_vec(&original)?,
    )?;
    assert_eq!(
        code(aggregate(&directory, &fixture.root)),
        Some("DUPLICATE_BETA_REPORT")
    );

    let directory = fixture.root.join("mixed");
    save(
        &directory.join("one.json"),
        &report("beta-001", "inspect", 10),
    )?;
    let mut other = report("beta-002", "inspect", 10);
    other.revision = "c".repeat(40);
    save(&directory.join("two.json"), &other)?;
    assert_eq!(
        code(aggregate(&directory, &fixture.root)),
        Some("MIXED_BETA_CANDIDATES")
    );

    let directory = fixture.root.join("empty");
    fs::create_dir(&directory)?;
    assert_eq!(
        code(aggregate(&directory, &fixture.root)),
        Some("NO_BETA_REPORTS")
    );
    Ok(())
}
