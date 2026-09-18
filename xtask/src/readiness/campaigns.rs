use super::{Policy, identity};
use crate::beta;
use crate::corpus::{self, Case};
use crate::release::TARGETS;
use crate::tooling::common::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn beta_evidence(
    directory: &Path,
    root: &Path,
    packages: &BTreeMap<String, String>,
    revision: &str,
    version: &str,
    minimum: u64,
) -> Result<()> {
    let summary = beta::aggregate(&directory.join("beta"), root)?;
    identity(&summary, "docsight.beta-summary/v1", revision, version)?;
    require(
        u64::try_from(array(field(&summary, "participants")?)?.len())
            .is_ok_and(|count| count >= minimum),
        "INSUFFICIENT_BETA_PARTICIPANTS",
        "Candidate lacks the required number of distinct beta participants",
    )?;
    let allowed = beta::known_diagnostics(root)?;
    for path in flat_files(&directory.join("beta"), "json", 10_000)? {
        let report = beta::validate_report(decode(read_json_limit(&path, 262_144)?)?, &allowed)?;
        require(
            packages.get(&report.target) == Some(&report.archive_sha256),
            "BETA_PACKAGE_MISMATCH",
            "Observation did not use a verified candidate package",
        )?;
    }
    Ok(())
}

pub fn corpus_evidence(
    directory: &Path,
    packages: &BTreeMap<String, String>,
    revision: &str,
    version: &str,
    policy: &Policy,
    root: &Path,
) -> Result<BTreeMap<String, Case>> {
    let path = directory.join("corpus-manifest.json");
    let taxonomy = crate::quality::load_taxonomy(root)?;
    let manifest = corpus::load_manifest(&path, None, &taxonomy)?;
    let manifest_hash = sha256_file(&path)?;
    let cases: BTreeMap<_, _> = manifest
        .cases
        .into_iter()
        .map(|case| (case.id.clone(), case))
        .collect();
    let mut documents = BTreeSet::new();
    let mut real = BTreeMap::from([("docx", BTreeSet::new()), ("pdf", BTreeSet::new())]);
    for case in cases.values() {
        if let Some(samples) = real.get_mut(case.format.as_str())
            && case.expected.exit_code == 0
        {
            documents.insert(case.sha256.clone());
            if case.origin == "consented-real" {
                samples.insert(case.sha256.clone());
            }
        }
    }
    require(
        u64::try_from(documents.len()).is_ok_and(|count| count >= policy.minimum_corpus_documents)
            && real.values().all(|samples| {
                u64::try_from(samples.len())
                    .is_ok_and(|count| count >= policy.minimum_real_documents_per_format)
            }),
        "INSUFFICIENT_REAL_CORPUS",
        "Synthetic or repeated cases cannot satisfy reviewed real-document thresholds",
    )?;
    for target in TARGETS {
        let raw = read_json_limit(&directory.join(format!("corpus-{target}.json")), 16_777_216)?;
        identity(&raw, "docsight.corpus-report/v1", revision, version)?;
        let report: corpus::Report = decode(raw)?;
        require(
            report.target == target
                && packages.get(target) == Some(&report.archive_sha256)
                && report.manifest_sha256 == manifest_hash
                && report.passed,
            "FAILED_CORPUS_CANDIDATE",
            "Every native corpus report must pass for the exact archive and manifest",
        )?;
        require(
            report.cases.len() == cases.len(),
            "MISSING_CORPUS_CASES",
            "Corpus report omitted required cases",
        )?;
        let mut seen = BTreeSet::new();
        for outcome in report.cases {
            let case = cases.get(&outcome.id).ok_or_else(|| {
                ToolError::new("MISSING_CORPUS_CASES", "A reviewed case was substituted")
            })?;
            require(
                seen.insert(outcome.id.clone()),
                "MISSING_CORPUS_CASES",
                "Corpus case was duplicated",
            )?;
            require(
                outcome.passed
                    && outcome.error_code.is_none()
                    && outcome.attempts == case.expected.repeat
                    && outcome.document_sha256 == case.sha256
                    && outcome.reference_sha256.as_ref()
                        == case.reference.as_ref().map(|input| &input.sha256)
                    && outcome.operation == case.operation
                    && outcome.origin == case.origin
                    && outcome.format == case.format
                    && outcome.class == case.class
                    && outcome
                        .output_sha256
                        .as_ref()
                        .is_some_and(|hash| is_hex(hash, 64))
                    && outcome.diagnostic_codes == case.expected.diagnostic_codes,
                "FAILED_CORPUS_CASE",
                "Corpus case lacks complete passing execution evidence",
            )?;
        }
    }
    Ok(cases)
}
