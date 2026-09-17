#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::corpus::{
    Expectation, Input, Manifest, Report, evaluate, load_manifest, run_corpus_with,
    validate_manifest,
};
use xtask::release::{TARGETS, native_target};
use xtask::tooling::common::{self, digest, is_hex, json_bytes, sha256_file, workspace_root};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Termination};

type Arguments = [OsString];
type Environment = Option<BTreeMap<OsString, OsString>>;

fn has(arguments: &Arguments, value: &str) -> bool {
    arguments.iter().any(|argument| argument == value)
}

fn expectation() -> Expectation {
    Expectation {
        exit_code: 0,
        diagnostic_codes: Vec::new(),
        pointer_equals: BTreeMap::new(),
        repeat: 1,
    }
}

fn error_envelope(code: &str, exit: i64) -> TestResult<ProcessResult> {
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
fn repository_corpus_manifest_is_hash_pinned_and_complete() -> TestResult {
    let root = workspace_root();
    let manifest = load_manifest(&root.join("release/corpus.json"), Some(&root))?;
    assert_eq!(manifest.cases.len(), 12);
    assert!(manifest.cases.iter().all(|case| case.origin == "synthetic"));
    assert!(
        manifest
            .cases
            .iter()
            .any(|case| case.expected.exit_code != 0 && !case.expected.diagnostic_codes.is_empty())
    );
    assert!(
        manifest
            .cases
            .iter()
            .filter(|case| case.operation == "diff")
            .all(|case| case.reference.is_some())
    );
    Ok(())
}

#[test]
fn invalid_manifests_fail_with_specific_codes() -> TestResult {
    let base = one_case("doc.docx", b"document", "inspect");
    let mut variants: Vec<(Manifest, &str)> = Vec::new();

    let mut manifest = base.clone();
    manifest.schema = "docsight.corpus/v2".into();
    variants.push((manifest, "INVALID_CORPUS_MANIFEST"));

    let mut manifest = base.clone();
    manifest.cases.clear();
    variants.push((manifest, "INVALID_CORPUS_MANIFEST"));

    let mut manifest = base.clone();
    manifest.cases.push(manifest.cases[0].clone());
    variants.push((manifest, "INVALID_CORPUS_ID"));

    let mut manifest = base.clone();
    manifest.cases[0].id = "Upper-Case".into();
    variants.push((manifest, "INVALID_CORPUS_ID"));

    let mut manifest = base.clone();
    manifest.cases[0].origin = "unreviewed".into();
    variants.push((manifest, "INVALID_CORPUS_VALUE"));

    let mut manifest = base.clone();
    manifest.cases[0].file = "../outside.docx".into();
    variants.push((manifest, "UNSAFE_ARCHIVE_PATH"));

    let mut manifest = base.clone();
    manifest.cases[0].expected.exit_code = 10;
    variants.push((manifest, "MISSING_CORPUS_ERROR"));

    let mut manifest = base.clone();
    manifest.cases[0].expected.diagnostic_codes = vec!["ZETA_CODE".into(), "ALPHA_CODE".into()];
    variants.push((manifest, "INVALID_CORPUS_DIAGNOSTICS"));

    let mut manifest = base.clone();
    manifest.cases[0].reference = Some(Input {
        file: "other.docx".into(),
        sha256: digest(b"other"),
    });
    variants.push((manifest, "UNUSED_CORPUS_REFERENCE"));

    for repeat in [0, 4] {
        let mut manifest = base.clone();
        manifest.cases[0].expected.repeat = repeat;
        variants.push((manifest, "INVALID_INTEGER"));
    }

    for (pointer, value) in [
        ("relative", json!(1)),
        ("/bad~2escape", json!(1)),
        ("/array", json!([1])),
        ("/object", json!({"a": 1})),
    ] {
        let mut manifest = base.clone();
        manifest.cases[0]
            .expected
            .pointer_equals
            .insert(pointer.into(), value);
        variants.push((manifest, "INVALID_CORPUS_ASSERTIONS"));
    }

    for (manifest, expected) in variants {
        assert_eq!(code(validate_manifest(manifest, None)), Some(expected));
    }
    Ok(())
}

#[test]
fn manifest_digests_are_checked_against_the_corpus_root() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join("doc.docx"), b"reviewed bytes")?;
    let manifest = one_case("doc.docx", b"reviewed bytes", "inspect");
    assert!(validate_manifest(manifest.clone(), Some(&fixture.root)).is_ok());
    fs::write(fixture.root.join("doc.docx"), b"changed bytes")?;
    assert_eq!(
        code(validate_manifest(manifest, Some(&fixture.root))),
        Some("CORPUS_DIGEST_MISMATCH")
    );
    Ok(())
}

#[test]
fn evaluation_accepts_clean_envelopes_with_matching_assertions() -> TestResult {
    let mut expected = expectation();
    expected
        .pointer_equals
        .insert("/result/pages".into(), json!(3));
    expected.diagnostic_codes = vec!["DOCX_FONT_SUBSTITUTED".into()];
    let result = outcome(
        0,
        json_bytes(&json!({
            "schema": "docsight.agent/v2",
            "result": {"pages": 3},
            "warnings": [{"code": "DOCX_FONT_SUBSTITUTED"}],
        }))?,
        Vec::new(),
    );
    assert_eq!(evaluate(&result, &expected)?, vec!["DOCX_FONT_SUBSTITUTED"]);
    Ok(())
}

#[test]
fn evaluation_distinguishes_every_failure_class() -> TestResult {
    let success = agent(json!({"pages": 3, "ratio": 1}))?;
    let mut assertion = expectation();
    assertion
        .pointer_equals
        .insert("/result/pages".into(), json!(4));
    assert_eq!(
        code(evaluate(&success, &assertion)),
        Some("CORPUS_ASSERTION")
    );

    let mut number_type = expectation();
    number_type
        .pointer_equals
        .insert("/result/ratio".into(), json!(1.0));
    assert_eq!(
        code(evaluate(&success, &number_type)),
        Some("CORPUS_ASSERTION")
    );

    let mut missing_pointer = expectation();
    missing_pointer
        .pointer_equals
        .insert("/result/absent".into(), json!(1));
    assert_eq!(
        code(evaluate(&success, &missing_pointer)),
        Some("CORPUS_MISSING_POINTER")
    );

    let mut diagnostic = expectation();
    diagnostic.diagnostic_codes = vec!["DOCX_FONT_SUBSTITUTED".into()];
    assert_eq!(
        code(evaluate(&success, &diagnostic)),
        Some("CORPUS_DIAGNOSTIC")
    );

    let mut noisy = success.clone();
    noisy.stderr = b"progress".to_vec();
    assert_eq!(
        code(evaluate(&noisy, &expectation())),
        Some("CORPUS_PROTOCOL")
    );

    let unversioned = outcome(0, json_bytes(&json!({"result": {}}))?, Vec::new());
    assert_eq!(
        code(evaluate(&unversioned, &expectation())),
        Some("CORPUS_PROTOCOL")
    );

    let mut limited = success.clone();
    limited.termination = Some(Termination::OutputLimit);
    assert_eq!(
        code(evaluate(&limited, &expectation())),
        Some("CORPUS_PROCESS_LIMIT")
    );

    let crashed = outcome(-6, Vec::new(), Vec::new());
    assert_eq!(
        code(evaluate(&crashed, &expectation())),
        Some("CORPUS_CRASH")
    );

    let mut negative = expectation();
    negative.exit_code = 10;
    negative.diagnostic_codes = vec!["UNSUPPORTED_FORMAT".into()];
    assert_eq!(
        code(evaluate(&success, &negative)),
        Some("CORPUS_EXIT_CODE")
    );
    assert_eq!(
        evaluate(&error_envelope("UNSUPPORTED_FORMAT", 10)?, &negative)?,
        vec!["UNSUPPORTED_FORMAT"]
    );
    let mut mislabeled = error_envelope("UNSUPPORTED_FORMAT", 10)?;
    mislabeled.stderr = json_bytes(&json!({
        "schema": "docsight.agent/v2",
        "error": {"code": "UNSUPPORTED_FORMAT", "exit_code": 11},
    }))?;
    assert_eq!(
        code(evaluate(&mislabeled, &negative)),
        Some("CORPUS_PROTOCOL")
    );
    let mut polluted = error_envelope("UNSUPPORTED_FORMAT", 10)?;
    polluted.stdout = b"partial".to_vec();
    assert_eq!(
        code(evaluate(&polluted, &negative)),
        Some("CORPUS_PROTOCOL")
    );
    Ok(())
}

struct Campaign {
    fixture: Fixture,
    archive: PathBuf,
    manifest: PathBuf,
}

impl Campaign {
    fn new(manifest: &Manifest, files: &[(&str, &[u8])]) -> TestResult<Self> {
        let fixture = Fixture::new()?;
        for (name, bytes) in files {
            let path = fixture.root.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, bytes)?;
        }
        let archive = fixture.package(native_target()?, "dist")?;
        let manifest_path = fixture.root.join("corpus-manifest.json");
        save(&manifest_path, manifest)?;
        Ok(Self {
            fixture,
            archive,
            manifest: manifest_path,
        })
    }

    fn run<F>(&self, mut engine: F) -> common::Result<Report>
    where
        F: FnMut(&Arguments, &Path, &ProcessLimits, Environment) -> common::Result<ProcessResult>,
    {
        let mut runner = Callback(
            |arguments: &Arguments,
             cwd: &Path,
             limits: &ProcessLimits,
             environment: Option<&BTreeMap<OsString, OsString>>| {
                engine(arguments, cwd, limits, environment.cloned())
            },
        );
        run_corpus_with(
            &self.archive,
            &self.manifest,
            &self.fixture.root,
            &mut runner,
        )
    }
}

fn synthetic(
    arguments: &Arguments,
    cwd: &Path,
    limits: &ProcessLimits,
    environment: Environment,
) -> common::Result<ProcessResult> {
    synthetic_engine(arguments, cwd, limits, environment.as_ref())
}

#[test]
fn passing_cases_are_repeated_in_the_sandbox_and_bound_to_the_candidate() -> TestResult {
    let manifest = one_case("cases/doc.docx", b"document", "inspect");
    let campaign = Campaign::new(&manifest, &[("cases/doc.docx", b"document")])?;
    let mut invocations = Vec::new();
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if !has(arguments, "--version") {
            invocations.push(arguments.to_vec());
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert!(report.passed);
    assert_eq!(report.revision, REVISION);
    assert_eq!(report.target, native_target()?);
    assert_eq!(report.archive_sha256, sha256_file(&campaign.archive)?);
    assert_eq!(report.manifest_sha256, sha256_file(&campaign.manifest)?);
    let case = &report.cases[0];
    assert_eq!(case.attempts, 2);
    assert!(
        case.output_sha256
            .as_deref()
            .is_some_and(|hash| is_hex(hash, 64))
    );
    assert_eq!(invocations.len(), 2);
    for arguments in &invocations {
        assert!(has(arguments, "--agent") && has(arguments, "--sandbox"));
        assert!(arguments
            .iter()
            .any(|argument| Path::new(argument) == campaign.fixture.root.join("cases/doc.docx")));
    }
    Ok(())
}

#[test]
fn repeated_commands_must_produce_identical_output_bytes() -> TestResult {
    let manifest = one_case("doc.docx", b"document", "inspect");
    let campaign = Campaign::new(&manifest, &[("doc.docx", b"document")])?;
    let mut calls = 0;
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "inspect") {
            calls += 1;
            return agent(json!({"format": "docx", "pages": calls}));
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert!(!report.passed);
    assert_eq!(
        report.cases[0].error_code.as_deref(),
        Some("CORPUS_NONDETERMINISTIC")
    );
    assert_eq!(report.cases[0].attempts, 2);
    Ok(())
}

#[test]
fn inputs_changed_during_a_command_invalidate_the_case() -> TestResult {
    let manifest = one_case("doc.docx", b"document", "inspect");
    let campaign = Campaign::new(&manifest, &[("doc.docx", b"document")])?;
    let document = campaign.fixture.root.join("doc.docx");
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "inspect") {
            fs::write(&document, b"swapped during execution")?;
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert_eq!(
        report.cases[0].error_code.as_deref(),
        Some("CORPUS_DIGEST_MISMATCH")
    );
    assert_eq!(report.cases[0].attempts, 1);
    Ok(())
}

#[test]
fn diff_cases_bind_both_reviewed_documents() -> TestResult {
    let mut manifest = one_case("before.docx", b"before", "diff");
    manifest.cases[0].reference = Some(Input {
        file: "after.docx".into(),
        sha256: digest(b"after"),
    });
    let campaign = Campaign::new(
        &manifest,
        &[("before.docx", b"before"), ("after.docx", b"after")],
    )?;
    let root = campaign.fixture.root.clone();
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "diff") {
            let tail: Vec<_> = arguments[arguments.len() - 2..]
                .iter()
                .map(PathBuf::from)
                .collect();
            if tail != [root.join("before.docx"), root.join("after.docx")] {
                return Err(common::ToolError::new(
                    "TEST_DIFF_ARGUMENTS",
                    "Unexpected diff inputs",
                ));
            }
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert!(report.passed, "{:?}", report.cases[0].error_code);
    assert_eq!(report.cases[0].reference_sha256, Some(digest(b"after")));

    let reference = campaign.fixture.root.join("after.docx");
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "diff") {
            fs::write(&reference, b"replaced reference")?;
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert_eq!(
        report.cases[0].error_code.as_deref(),
        Some("CORPUS_DIGEST_MISMATCH")
    );
    Ok(())
}

#[test]
fn render_cases_require_a_valid_png_artifact() -> TestResult {
    let manifest = one_case("doc.docx", b"document", "render");
    let campaign = Campaign::new(&manifest, &[("doc.docx", b"document")])?;
    assert!(campaign.run(synthetic)?.passed);

    let report = campaign.run(|arguments, cwd, limits, environment| {
        let result = synthetic(arguments, cwd, limits, environment)?;
        if has(arguments, "render") {
            let index = arguments
                .iter()
                .position(|argument| argument == "--out")
                .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output"))?;
            fs::write(&arguments[index + 1], b"not a png")?;
        }
        Ok(result)
    })?;
    assert_eq!(
        report.cases[0].error_code.as_deref(),
        Some("SMOKE_INVALID_PNG")
    );
    Ok(())
}

#[test]
fn a_failing_case_does_not_hide_the_remaining_cases() -> TestResult {
    let mut manifest = one_case("broken.docx", b"broken", "inspect");
    let mut healthy = manifest.cases[0].clone();
    healthy.id = "healthy-case".into();
    healthy.file = "healthy.docx".into();
    healthy.sha256 = digest(b"healthy");
    manifest.cases[0].id = "broken-case".into();
    manifest.cases.push(healthy);
    let campaign = Campaign::new(
        &manifest,
        &[("broken.docx", b"broken"), ("healthy.docx", b"healthy")],
    )?;
    let report = campaign.run(|arguments, cwd, limits, environment| {
        if arguments
            .iter()
            .any(|argument| argument.to_string_lossy().ends_with("broken.docx"))
        {
            return error_envelope("MALFORMED_DOCUMENT", 11).map_err(|_| {
                common::ToolError::new("TEST_ENVELOPE", "Cannot build test envelope")
            });
        }
        synthetic(arguments, cwd, limits, environment)
    })?;
    assert!(!report.passed);
    assert_eq!(report.cases.len(), 2);
    assert_eq!(
        report.cases[0].error_code.as_deref(),
        Some("CORPUS_EXIT_CODE")
    );
    assert!(report.cases[1].passed);
    Ok(())
}

#[test]
fn candidate_identity_and_evidence_changes_abort_the_run() -> TestResult {
    let manifest = one_case("doc.docx", b"document", "inspect");
    let campaign = Campaign::new(&manifest, &[("doc.docx", b"document")])?;
    let wrong_version = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "--version") {
            return Ok(outcome(0, b"docsight 0.0.0\n".to_vec(), Vec::new()));
        }
        synthetic(arguments, cwd, limits, environment)
    });
    assert_eq!(code(wrong_version), Some("CORPUS_VERSION_MISMATCH"));

    let manifest_path = campaign.manifest.clone();
    let changed = campaign.run(|arguments, cwd, limits, environment| {
        if has(arguments, "inspect") {
            let mut bytes = fs::read(&manifest_path)?;
            bytes.push(b'\n');
            fs::write(&manifest_path, bytes)?;
        }
        synthetic(arguments, cwd, limits, environment)
    });
    assert_eq!(code(changed), Some("CORPUS_EVIDENCE_CHANGED"));
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
    fs::write(fixture.root.join("doc.docx"), b"document")?;
    let archive = fixture.package(foreign, "dist")?;
    let manifest_path = fixture.root.join("corpus-manifest.json");
    save(
        &manifest_path,
        &one_case("doc.docx", b"document", "inspect"),
    )?;
    let mut calls = 0;
    let mut runner = Callback(
        |_: &Arguments, _: &Path, _: &ProcessLimits, _: Option<&BTreeMap<OsString, OsString>>| {
            calls += 1;
            Ok(version())
        },
    );
    assert_eq!(
        code(run_corpus_with(
            &archive,
            &manifest_path,
            &fixture.root,
            &mut runner
        )),
        Some("CORPUS_HOST_MISMATCH")
    );
    assert_eq!(calls, 0);
    Ok(())
}
