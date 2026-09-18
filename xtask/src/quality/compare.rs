use super::measure::{DocumentResult, QualityReport, validate_report};
use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const COMPARISON_SCHEMA: &str = "docsight.quality-comparison/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub document_sha256: String,
    pub class: String,
    pub metric: String,
    pub baseline: String,
    pub candidate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub revision: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    pub schema: String,
    pub baseline: Candidate,
    pub candidate: Candidate,
    pub regressions: Vec<Change>,
    pub improvements: Vec<Change>,
    pub removed_documents: Vec<String>,
    pub added_documents: Vec<String>,
    pub passed: bool,
}

/// Compares two measurements of the same documents. A metric that passed and now fails or
/// is no longer measured, a document that started failing to measure, and a document that
/// disappeared are regressions; the reverse transitions are improvements.
pub fn compare(baseline: &QualityReport, candidate: &QualityReport) -> Result<Comparison> {
    validate_report(baseline)?;
    validate_report(candidate)?;
    let index = |report: &QualityReport| -> BTreeMap<(String, String), DocumentResult> {
        report
            .documents
            .iter()
            .map(|document| {
                (
                    (document.document_sha256.clone(), document.class.clone()),
                    document.clone(),
                )
            })
            .collect()
    };
    let before = index(baseline);
    let after = index(candidate);
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    for (key, old) in &before {
        let Some(new) = after.get(key) else {
            continue;
        };
        let old_state = document_state(old);
        let new_state = document_state(new);
        if old_state != new_state {
            let change = Change {
                document_sha256: key.0.clone(),
                class: key.1.clone(),
                metric: "measurement".to_owned(),
                baseline: old_state.to_owned(),
                candidate: new_state.to_owned(),
            };
            if new_state == "error" {
                regressions.push(change);
            } else {
                improvements.push(change);
            }
        }
        for (metric, outcome) in &old.metrics {
            let Some(current) = new.metrics.get(metric) else {
                continue;
            };
            if outcome.status == current.status && outcome.basis == current.basis {
                continue;
            }
            let change = Change {
                document_sha256: key.0.clone(),
                class: key.1.clone(),
                metric: metric.clone(),
                baseline: format!("{}:{}", outcome.basis, outcome.status),
                candidate: format!("{}:{}", current.basis, current.status),
            };
            let lost = outcome.status == "passed" && current.status != "passed";
            let started_failing = outcome.status != "failed" && current.status == "failed";
            let recovered = outcome.status == "failed" && current.status == "passed";
            let gained = outcome.status == "not_measured" && current.status == "passed";
            if lost || started_failing {
                regressions.push(change);
            } else if recovered || gained {
                improvements.push(change);
            }
        }
    }
    let removed_documents: Vec<String> = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .map(|(sha256, class)| format!("{sha256}:{class}"))
        .collect();
    let added_documents: Vec<String> = after
        .keys()
        .filter(|key| !before.contains_key(*key))
        .map(|(sha256, class)| format!("{sha256}:{class}"))
        .collect();
    let summary = |report: &QualityReport| Candidate {
        revision: report.revision.clone(),
        archive_sha256: report.archive_sha256.clone(),
        manifest_sha256: report.manifest_sha256.clone(),
    };
    Ok(Comparison {
        schema: COMPARISON_SCHEMA.to_owned(),
        baseline: summary(baseline),
        candidate: summary(candidate),
        passed: regressions.is_empty() && removed_documents.is_empty(),
        regressions,
        improvements,
        removed_documents,
        added_documents,
    })
}

fn document_state(document: &DocumentResult) -> &'static str {
    if document.error_code.is_some() {
        "error"
    } else {
        "measured"
    }
}
