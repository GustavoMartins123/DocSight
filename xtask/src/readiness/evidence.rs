use super::identity;
use crate::release::{
    SmokeReceipt, TARGETS,
    archive::{verify_archive, verify_sidecar},
    validate_smoke_receipt,
};
use crate::tooling::common::*;
use crate::validation::{CHECK_NAMES, Report as ValidationReport, Status};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub file: String,
    pub sha256: String,
}

pub fn bound_file(root: &Path, reference: &Reference) -> Result<PathBuf> {
    checked_digest(&reference.sha256)?;
    let path = contained_file(root, &reference.file)?;
    require(
        sha256_file(&path)? == reference.sha256,
        "EVIDENCE_DIGEST_MISMATCH",
        "Evidence changed after it was recorded",
    )?;
    Ok(path)
}

pub fn validate_workspace(directory: &Path, revision: &str, version: &str) -> Result<()> {
    let raw = read_json(&directory.join("validation/validation.json"))?;
    identity(&raw, "docsight.validation/v2", revision, version)?;
    let report: ValidationReport = decode(raw)?;
    require(
        report.passed
            && report.clean_tree_before
            && report.clean_tree_after
            && report.same_revision_after,
        "INCOMPLETE_WORKSPACE_VALIDATION",
        "Every workspace gate must pass on the same clean revision",
    )?;
    require(
        report
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .eq(CHECK_NAMES),
        "MISSING_VALIDATION_GATES",
        "Workspace validation omitted or duplicated a required gate",
    )?;
    for check in &report.checks {
        require(
            check.status == Status::Pass && check.exit_code == Some(0) && check.reason.is_none(),
            "FAILED_VALIDATION_GATE",
            "A required workspace gate did not pass",
        )?;
        for (file, sha256) in [
            (&check.stdout_log, &check.stdout_sha256),
            (&check.stderr_log, &check.stderr_sha256),
        ] {
            bound_file(
                &directory.join("validation"),
                &Reference {
                    file: file.clone(),
                    sha256: sha256.clone(),
                },
            )?;
        }
    }
    Ok(())
}

pub fn package_evidence(
    directory: &Path,
    revision: &str,
    version: &str,
) -> Result<BTreeMap<String, String>> {
    let paths = flat_files(directory, "zip", 10_000)?;
    require(
        paths.len() == TARGETS.len(),
        "MISSING_NATIVE_PACKAGES",
        "Readiness requires five native package archives",
    )?;
    let mut found = BTreeMap::new();
    let mut toolchains = BTreeSet::new();
    for path in paths {
        let manifest = verify_archive(&path)?;
        require(
            manifest.revision == revision && manifest.version == version,
            "EVIDENCE_CANDIDATE_MISMATCH",
            "Package does not describe the exact candidate",
        )?;
        require(
            !found.contains_key(&manifest.target),
            "DUPLICATE_NATIVE_PACKAGE",
            "Each target must occur exactly once",
        )?;
        let hash = verify_sidecar(&path)?;
        let receipt: SmokeReceipt = decode(read_json(
            &directory.join(format!("smoke-{}.json", manifest.target)),
        )?)?;
        validate_smoke_receipt(&receipt, &manifest, &hash)?;
        found.insert(manifest.target, hash);
        toolchains.insert(manifest.toolchain);
    }
    require(
        found.len() == TARGETS.len()
            && TARGETS.iter().all(|target| found.contains_key(*target))
            && toolchains.len() == 1,
        "MISSING_NATIVE_PACKAGES",
        "Readiness requires the exact native platform matrix",
    )?;
    Ok(found)
}
