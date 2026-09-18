#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use support::*;
use xtask::release::archive::verify_archive;
use xtask::release::distribution::{POLICY_PATH, Policy, ProvenanceStatus, load_policy};
use xtask::release::provenance::{
    ProvenanceReceipt, ProvenanceResult, SLSA_PROVENANCE, validate_provenance_receipt,
    verify_provenance_with,
};
use xtask::tooling::common::{ToolError, sha256_file};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Termination};

const LINUX: &str = "x86_64-unknown-linux-gnu";

type Mutation = Box<dyn Fn(&mut Value)>;

fn require_provenance(fixture: &Fixture) -> TestResult {
    let path = fixture.root.join(POLICY_PATH);
    let mut policy: Policy = serde_json::from_slice(&fs::read(&path)?)?;
    policy.provenance.status = ProvenanceStatus::Required;
    fs::remove_file(&path)?;
    save(&path, &policy)
}

struct Gh {
    code: i64,
    answer: Value,
    timeout: bool,
}

fn run_gh(
    fixture: &Fixture,
    archive: &Path,
    gh: Gh,
    calls: &mut Vec<Vec<OsString>>,
) -> TestResult<ProvenanceReceipt> {
    let mut runner = Callback(
        |arguments: &[OsString],
         _: &Path,
         _: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>|
         -> xtask::tooling::common::Result<ProcessResult> {
            if environment.is_some() {
                return Err(ToolError::new(
                    "UNEXPECTED_ENVIRONMENT",
                    "gh must inherit its token",
                ));
            }
            calls.push(arguments.to_vec());
            let mut result = outcome(
                gh.code,
                serde_json::to_vec(&gh.answer)
                    .map_err(|_| ToolError::new("TEST_JSON", "cannot encode"))?,
                Vec::new(),
            );
            if gh.timeout {
                result.termination = Some(Termination::Timeout);
            }
            Ok(result)
        },
    );
    Ok(verify_provenance_with(
        archive,
        &fixture.root,
        Path::new("/usr/bin/gh"),
        &mut runner,
    )?)
}

#[test]
fn pending_provenance_is_recorded_without_contacting_github() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let receipt = verify_provenance_with(
        &archive,
        &fixture.root,
        Path::new("/usr/bin/gh"),
        &mut Callback(refuse_processes),
    )?;
    assert_eq!(receipt.status, ProvenanceResult::PendingAttestationSupport);
    assert_eq!(receipt.error_code, None);
    assert!(receipt.attestations.is_empty());
    assert_eq!(receipt.repository, REPOSITORY);
    assert_eq!(receipt.signer_workflow, SIGNER);
    let loaded = load_policy(&fixture.root)?;
    validate_provenance_receipt(
        &receipt,
        &verify_archive(&archive)?,
        &sha256_file(&archive)?,
        &loaded,
    )?;
    Ok(())
}

#[test]
fn required_provenance_binds_archive_repository_workflow_and_revision() -> TestResult {
    let fixture = Fixture::new()?;
    require_provenance(&fixture)?;
    let archive = fixture.package(LINUX, "dist")?;
    let digest = sha256_file(&archive)?;
    let mut calls = Vec::new();
    let receipt = run_gh(
        &fixture,
        &archive,
        Gh {
            code: 0,
            answer: verification(&digest),
            timeout: false,
        },
        &mut calls,
    )?;
    assert_eq!(receipt.status, ProvenanceResult::Verified, "{receipt:?}");
    assert_eq!(receipt.attestations.len(), 1);
    assert_eq!(receipt.attestations[0].source_digest, REVISION);
    assert_eq!(receipt.attestations[0].runner_environment, "github-hosted");
    assert_eq!(receipt.attestations[0].transparency_entries, 1);
    let call: Vec<String> = calls[0]
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let archive_text = archive.to_string_lossy().into_owned();
    assert_eq!(
        call,
        [
            "/usr/bin/gh",
            "attestation",
            "verify",
            archive_text.as_str(),
            "--repo",
            REPOSITORY,
            "--signer-workflow",
            SIGNER,
            "--source-digest",
            REVISION,
            "--predicate-type",
            SLSA_PROVENANCE,
            "--deny-self-hosted-runners",
            "--format",
            "json",
        ]
    );
    let loaded = load_policy(&fixture.root)?;
    let manifest = verify_archive(&archive)?;
    validate_provenance_receipt(&receipt, &manifest, &digest, &loaded)?;
    Ok(())
}

#[test]
fn provenance_fails_closed_when_any_binding_differs() -> TestResult {
    let fixture = Fixture::new()?;
    require_provenance(&fixture)?;
    let archive = fixture.package(LINUX, "dist")?;
    let digest = sha256_file(&archive)?;
    let certificate = "/0/verificationResult/signature/certificate";
    let mutations: Vec<(&str, Mutation)> = vec![
        (
            "source digest",
            Box::new(move |value| {
                value[0]["verificationResult"]["signature"]["certificate"]["sourceRepositoryDigest"] =
                    json!("b".repeat(40));
            }),
        ),
        (
            "repository",
            Box::new(move |value| {
                if let Some(field) =
                    value.pointer_mut(&format!("{certificate}/sourceRepositoryURI"))
                {
                    *field = json!("https://github.com/someone/fork");
                }
            }),
        ),
        (
            "signer",
            Box::new(move |value| {
                if let Some(field) =
                    value.pointer_mut(&format!("{certificate}/subjectAlternativeName"))
                {
                    *field = json!(format!(
                        "https://github.com/{REPOSITORY}/.github/workflows/other.yml@refs/heads/main"
                    ));
                }
            }),
        ),
        (
            "runner",
            Box::new(move |value| {
                if let Some(field) = value.pointer_mut(&format!("{certificate}/runnerEnvironment"))
                {
                    *field = json!("self-hosted");
                }
            }),
        ),
        (
            "subject",
            Box::new(|value| {
                value[0]["verificationResult"]["statement"]["subject"][1]["digest"]["sha256"] =
                    json!("c".repeat(64));
            }),
        ),
        (
            "predicate",
            Box::new(|value| {
                value[0]["verificationResult"]["statement"]["predicateType"] =
                    json!("https://example.com/other");
            }),
        ),
        (
            "timestamps",
            Box::new(|value| {
                value[0]["verificationResult"]["verifiedTimestamps"] = json!([]);
            }),
        ),
    ];
    for (name, mutate) in mutations {
        let mut answer = verification(&digest);
        mutate(&mut answer);
        let receipt = run_gh(
            &fixture,
            &archive,
            Gh {
                code: 0,
                answer,
                timeout: false,
            },
            &mut Vec::new(),
        )?;
        assert_eq!(receipt.status, ProvenanceResult::Failed, "{name}");
        assert_eq!(
            receipt.error_code.as_deref(),
            Some("PROVENANCE_MISMATCH"),
            "{name}"
        );
        assert!(receipt.attestations.is_empty());
    }
    for (gh, expected) in [
        (
            Gh {
                code: 1,
                answer: verification(&digest),
                timeout: false,
            },
            "PROVENANCE_UNVERIFIED",
        ),
        (
            Gh {
                code: 0,
                answer: json!([]),
                timeout: false,
            },
            "PROVENANCE_UNVERIFIED",
        ),
        (
            Gh {
                code: 0,
                answer: verification(&digest),
                timeout: true,
            },
            "PROVENANCE_VERIFIER_LIMIT",
        ),
    ] {
        let receipt = run_gh(&fixture, &archive, gh, &mut Vec::new())?;
        assert_eq!(receipt.error_code.as_deref(), Some(expected));
    }
    Ok(())
}

#[test]
fn provenance_receipts_must_match_the_candidate_and_the_policy() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let digest = sha256_file(&archive)?;
    let manifest = verify_archive(&archive)?;
    let pending = verify_provenance_with(
        &archive,
        &fixture.root,
        Path::new("/usr/bin/gh"),
        &mut Callback(refuse_processes),
    )?;
    let loaded = load_policy(&fixture.root)?;
    let mut wrong_archive = pending.clone();
    wrong_archive.archive_sha256 = "f".repeat(64);
    assert_eq!(
        code(validate_provenance_receipt(
            &wrong_archive,
            &manifest,
            &digest,
            &loaded
        )),
        Some("INVALID_PROVENANCE_RECEIPT")
    );
    let mut wrong_workflow = pending.clone();
    wrong_workflow.signer_workflow = format!("{REPOSITORY}/.github/workflows/other.yml");
    assert_eq!(
        code(validate_provenance_receipt(
            &wrong_workflow,
            &manifest,
            &digest,
            &loaded
        )),
        Some("INVALID_PROVENANCE_RECEIPT")
    );
    let mut claimed = pending;
    claimed.status = ProvenanceResult::Verified;
    assert_eq!(
        code(validate_provenance_receipt(
            &claimed, &manifest, &digest, &loaded
        )),
        Some("PROVENANCE_POLICY_UNMET")
    );

    require_provenance(&fixture)?;
    let required = load_policy(&fixture.root)?;
    let mut empty = claimed.clone();
    empty.policy_sha256 = required.sha256.clone();
    assert_eq!(
        code(validate_provenance_receipt(
            &empty, &manifest, &digest, &required
        )),
        Some("PROVENANCE_POLICY_UNMET")
    );
    Ok(())
}
