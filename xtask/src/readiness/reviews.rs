use super::{
    REVIEWS,
    evidence::{Reference, bound_file},
    identity,
};
use crate::corpus::{Case, Report as CorpusReport};
use crate::tooling::common::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    approved: bool,
    reviewer: String,
    evidence: Reference,
}

pub fn review_evidence(directory: &Path, revision: &str, version: &str, root: &Path) -> Result<()> {
    let value = read_json(&directory.join("reviews.json"))?;
    exact_keys(
        &value,
        &["schema", "version", "revision", "policy_sha256", "reviews"],
    )?;
    identity(&value, "docsight.release-reviews/v1", revision, version)?;
    require(
        string(field(&value, "policy_sha256")?)?
            == sha256_file(&root.join("release/readiness-policy.json"))?,
        "UNREVIEWED_READINESS_POLICY",
        "Reviewers must approve the exact acceptance thresholds",
    )?;
    let reviews: BTreeMap<String, Review> = decode(field(&value, "reviews")?.clone())?;
    require(
        reviews.len() == REVIEWS.len() && REVIEWS.iter().all(|name| reviews.contains_key(*name)),
        "MISSING_MANUAL_REVIEWS",
        "Every criterion requires a recorded independent review",
    )?;
    for review in reviews.values() {
        require(
            review.approved
                && (3..=80).contains(&review.reviewer.len())
                && review
                    .reviewer
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "PENDING_MANUAL_REVIEW",
            "A named reviewer has not approved a required criterion",
        )?;
        let path = bound_file(directory, &review.evidence)?;
        let bytes = read_bytes(&path, 1_048_576)?;
        require(
            bytes.len() >= 100,
            "EMPTY_MANUAL_REVIEW",
            "Review evidence must contain procedures, observations and limitations",
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    id: String,
    severity: String,
    status: String,
    regression_case: Option<String>,
    before_evidence: Option<Reference>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issues {
    schema: String,
    issues: Vec<Issue>,
}

pub fn regression_evidence(
    directory: &Path,
    cases: &BTreeMap<String, Case>,
    root: &Path,
    revision: &str,
) -> Result<()> {
    let register: Issues = decode(read_json(&root.join("release/beta-issues.json"))?)?;
    require(
        register.schema == "docsight.beta-issues/v1",
        "INVALID_BETA_ISSUES",
        "Beta issue register must be versioned",
    )?;
    let mut seen = BTreeSet::new();
    for issue in register.issues {
        require(
            !issue.id.is_empty()
                && seen.insert(issue.id)
                && ["open", "resolved", "deferred"].contains(&issue.status.as_str())
                && ["blocker", "high", "medium", "low"].contains(&issue.severity.as_str()),
            "INVALID_BETA_ISSUES",
            "Issue identities, status and severity must be explicit",
        )?;
        if issue.status != "resolved" {
            require(
                !["blocker", "high"].contains(&issue.severity.as_str()),
                "OPEN_BETA_BLOCKER",
                "A high-severity beta issue remains unresolved",
            )?;
            continue;
        }
        let case = issue
            .regression_case
            .as_ref()
            .and_then(|id| cases.get(id))
            .ok_or_else(|| {
                ToolError::new(
                    "MISSING_BETA_REGRESSION",
                    "Resolved issue requires a passing regression case",
                )
            })?;
        let reference = issue.before_evidence.ok_or_else(|| {
            ToolError::new(
                "MISSING_PRE_FIX_FAILURE",
                "Resolved issue requires pre-fix evidence",
            )
        })?;
        let before: CorpusReport = decode(read_json_limit(
            &bound_file(directory, &reference)?,
            16_777_216,
        )?)?;
        checked_revision(&before.revision)?;
        checked_version(&before.version)?;
        crate::release::checked_target(&before.target)?;
        checked_digest(&before.archive_sha256)?;
        checked_digest(&before.manifest_sha256)?;
        require(
            !before.passed
                && before.schema == "docsight.corpus-report/v1"
                && before.revision != revision,
            "MISSING_PRE_FIX_FAILURE",
            "Regression evidence must preserve a distinct pre-fix execution",
        )?;
        let failures: Vec<_> = before
            .cases
            .iter()
            .filter(|outcome| outcome.id == case.id)
            .collect();
        require(
            failures.len() == 1,
            "MISSING_PRE_FIX_FAILURE",
            "Pre-fix evidence must identify the case exactly once",
        )?;
        let failure = failures[0];
        let eligible = [
            "CORPUS_ASSERTION",
            "CORPUS_DIAGNOSTIC",
            "CORPUS_CRASH",
            "CORPUS_EXIT_CODE",
            "CORPUS_NONDETERMINISTIC",
            "CORPUS_PROCESS_LIMIT",
            "SMOKE_INVALID_PNG",
        ];
        require(
            !failure.passed
                && (1..=3).contains(&failure.attempts)
                && failure.document_sha256 == case.sha256
                && failure.operation == case.operation
                && failure.origin == case.origin
                && failure.format == case.format
                && failure.reference_sha256.as_ref()
                    == case.reference.as_ref().map(|input| &input.sha256)
                && failure
                    .error_code
                    .as_ref()
                    .is_some_and(|code| eligible.contains(&code.as_str())),
            "MISSING_PRE_FIX_FAILURE",
            "Issue must have a reproduced engine failure before the fix",
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gap {
    id: String,
    blocking: bool,
    status: String,
    source: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gaps {
    schema: String,
    items: Vec<Gap>,
}

pub fn known_gaps(root: &Path) -> Result<()> {
    let register: Gaps = decode(read_json(&root.join("release/known-gaps.json"))?)?;
    require(
        register.schema == "docsight.known-gaps/v1",
        "INVALID_KNOWN_GAPS",
        "Known gaps must remain versioned",
    )?;
    let mut seen = BTreeSet::new();
    let mut blocking = false;
    for gap in register.items {
        require(
            !gap.id.is_empty()
                && seen.insert(gap.id)
                && ["open", "resolved", "deferred-until-real-case"].contains(&gap.status.as_str()),
            "INVALID_KNOWN_GAPS",
            "Known gap identities and states must be explicit",
        )?;
        contained_file(root, &gap.source)?;
        blocking |= gap.blocking && gap.status != "resolved";
    }
    require(
        !blocking,
        "OPEN_V1_PRODUCT_GAPS",
        "Unresolved v1-blocking engine gaps remain declared",
    )
}
