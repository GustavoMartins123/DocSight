use super::archive::{Manifest, verify_archive};
use super::distribution::{LoadedPolicy, ProvenanceStatus, load_policy};
use crate::tooling::common::*;
use crate::tooling::process::{ProcessLimits, Runner};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

pub const PROVENANCE_SCHEMA: &str = "docsight.release-provenance/v1";
pub const SLSA_PROVENANCE: &str = "https://slsa.dev/provenance/v1";
const MAX_ATTESTATIONS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceResult {
    Verified,
    PendingAttestationSupport,
    Failed,
}

impl ProvenanceResult {
    pub fn expected(status: ProvenanceStatus) -> Self {
        match status {
            ProvenanceStatus::Required => Self::Verified,
            ProvenanceStatus::PendingAttestationSupport => Self::PendingAttestationSupport,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub signer: String,
    pub source_digest: String,
    pub source_ref: String,
    pub runner_environment: String,
    pub run: String,
    pub transparency_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceReceipt {
    pub schema: String,
    pub version: String,
    pub target: String,
    pub revision: String,
    pub archive_sha256: String,
    pub policy_sha256: String,
    pub repository: String,
    pub signer_workflow: String,
    pub status: ProvenanceResult,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<String>,
    pub attestations: Vec<Attestation>,
}

fn mismatch() -> ToolError {
    ToolError::new(
        "PROVENANCE_MISMATCH",
        "Attestation does not bind this archive to the policy repository, workflow and revision",
    )
}

fn text_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(mismatch)
}

fn attestation(
    entry: &Value,
    archive_sha256: &str,
    repository: &str,
    signer_workflow: &str,
    revision: &str,
) -> Result<Attestation> {
    let result = entry.get("verificationResult").ok_or_else(mismatch)?;
    let certificate = result
        .pointer("/signature/certificate")
        .ok_or_else(mismatch)?;
    let subjects = result
        .pointer("/statement/subject")
        .and_then(Value::as_array)
        .ok_or_else(mismatch)?;
    let signer = text_at(certificate, "/subjectAlternativeName")?;
    let source_digest = text_at(certificate, "/sourceRepositoryDigest")?;
    let runner_environment = text_at(certificate, "/runnerEnvironment")?;
    let transparency_entries = result
        .get("verifiedTimestamps")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    require(
        text_at(result, "/statement/predicateType")? == SLSA_PROVENANCE
            && subjects.iter().any(|subject| {
                subject.pointer("/digest/sha256").and_then(Value::as_str) == Some(archive_sha256)
            })
            && text_at(certificate, "/sourceRepositoryURI")?
                == format!("https://github.com/{repository}")
            && signer.starts_with(&format!("https://github.com/{signer_workflow}@"))
            && source_digest == revision
            && runner_environment == "github-hosted"
            && transparency_entries > 0,
        "PROVENANCE_MISMATCH",
        "Attestation does not bind this archive to the policy repository, workflow and revision",
    )?;
    Ok(Attestation {
        signer: signer.to_owned(),
        source_digest: source_digest.to_owned(),
        source_ref: text_at(certificate, "/sourceRepositoryRef")?.to_owned(),
        runner_environment: runner_environment.to_owned(),
        run: text_at(certificate, "/runInvocationURI")?.to_owned(),
        transparency_entries: u64::try_from(transparency_entries).map_err(|_| mismatch())?,
    })
}

fn verify_attestations<R: Runner>(
    archive: &Path,
    manifest: &Manifest,
    archive_sha256: &str,
    loaded: &LoadedPolicy,
    gh: &Path,
    runner: &mut R,
) -> Result<Vec<Attestation>> {
    let provenance = &loaded.policy.provenance;
    let signer_workflow = format!("{}/{}", provenance.repository, provenance.signer_workflow);
    let arguments: Vec<OsString> = vec![
        gh.as_os_str().to_owned(),
        "attestation".into(),
        "verify".into(),
        archive.as_os_str().to_owned(),
        "--repo".into(),
        provenance.repository.clone().into(),
        "--signer-workflow".into(),
        signer_workflow.clone().into(),
        "--source-digest".into(),
        manifest.revision.clone().into(),
        "--predicate-type".into(),
        SLSA_PROVENANCE.into(),
        "--deny-self-hosted-runners".into(),
        "--format".into(),
        "json".into(),
    ];
    let work = archive
        .parent()
        .ok_or_else(|| ToolError::new("INVALID_ARCHIVE", "Archive must have a parent directory"))?;
    let result = runner.run(
        &arguments,
        work,
        &ProcessLimits {
            timeout: Duration::from_secs(180),
            output_bytes: 16_777_216,
        },
        None,
    )?;
    require(
        result.termination.is_none(),
        "PROVENANCE_VERIFIER_LIMIT",
        "Attestation verifier exceeded its execution budget",
    )?;
    require(
        result.returncode == 0,
        "PROVENANCE_UNVERIFIED",
        "GitHub CLI did not verify an attestation for this archive and identity",
    )?;
    let entries = parse_json(&result.stdout)?;
    let entries = entries.as_array().ok_or_else(mismatch)?;
    require(
        (1..=MAX_ATTESTATIONS).contains(&entries.len()),
        "PROVENANCE_UNVERIFIED",
        "Verification returned no attestation or more than its limit",
    )?;
    entries
        .iter()
        .map(|entry| {
            attestation(
                entry,
                archive_sha256,
                &provenance.repository,
                &signer_workflow,
                &manifest.revision,
            )
        })
        .collect()
}

pub fn verify_provenance_with<R: Runner>(
    archive: &Path,
    root: &Path,
    gh: &Path,
    runner: &mut R,
) -> Result<ProvenanceReceipt> {
    let loaded = load_policy(root)?;
    let manifest = verify_archive(archive)?;
    let archive_sha256 = sha256_file(archive)?;
    let provenance = &loaded.policy.provenance;
    let (status, error_code, attestations) = match provenance.status {
        ProvenanceStatus::PendingAttestationSupport => (
            ProvenanceResult::PendingAttestationSupport,
            None,
            Vec::new(),
        ),
        ProvenanceStatus::Required => {
            match verify_attestations(archive, &manifest, &archive_sha256, &loaded, gh, runner) {
                Ok(attestations) => (ProvenanceResult::Verified, None, attestations),
                Err(error) => (
                    ProvenanceResult::Failed,
                    Some(error.code.to_owned()),
                    Vec::new(),
                ),
            }
        }
    };
    require(
        sha256_file(archive)? == archive_sha256,
        "ARCHIVE_CHANGED",
        "Archive changed during provenance verification",
    )?;
    Ok(ProvenanceReceipt {
        schema: PROVENANCE_SCHEMA.into(),
        version: manifest.version,
        target: manifest.target,
        revision: manifest.revision,
        archive_sha256,
        policy_sha256: loaded.sha256.clone(),
        repository: provenance.repository.clone(),
        signer_workflow: format!("{}/{}", provenance.repository, provenance.signer_workflow),
        status,
        error_code,
        attestations,
    })
}

pub fn validate_provenance_receipt(
    receipt: &ProvenanceReceipt,
    manifest: &Manifest,
    archive_sha256: &str,
    loaded: &LoadedPolicy,
) -> Result<()> {
    let provenance = &loaded.policy.provenance;
    require(
        receipt.schema == PROVENANCE_SCHEMA
            && receipt.version == manifest.version
            && receipt.target == manifest.target
            && receipt.revision == manifest.revision
            && receipt.archive_sha256 == archive_sha256
            && receipt.policy_sha256 == loaded.sha256
            && receipt.repository == provenance.repository
            && receipt.signer_workflow
                == format!("{}/{}", provenance.repository, provenance.signer_workflow),
        "INVALID_PROVENANCE_RECEIPT",
        "Provenance receipt does not attest this exact candidate archive and policy",
    )?;
    require(
        receipt.status == ProvenanceResult::expected(provenance.status)
            && receipt.error_code.is_none()
            && (receipt.status != ProvenanceResult::Verified
                || (!receipt.attestations.is_empty()
                    && receipt
                        .attestations
                        .iter()
                        .all(|attestation| attestation.source_digest == manifest.revision))),
        "PROVENANCE_POLICY_UNMET",
        "Provenance verification does not satisfy the distribution policy",
    )
}
