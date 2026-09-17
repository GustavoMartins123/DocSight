pub mod campaigns;
pub mod evidence;
pub mod reviews;

use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const REVIEWS: [&str; 6] = ["policy", "installation", "behavior-and-json", "render-and-diff", "security-and-fuzzing", "beta-participation-and-triage"];
pub const CRITERIA: [&str; 7] = ["workspace-validation", "five-native-packages", "beta-observations", "broad-corpus", "manual-reviews", "beta-regressions", "known-v1-gaps"];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy { pub schema: String, pub minimum_beta_participants: u64, pub minimum_corpus_documents: u64, pub minimum_real_documents_per_format: u64, pub required_reviews: Vec<String>, }

pub fn load_policy(root: &Path) -> Result<Policy> {
    let policy: Policy = decode(read_json(&root.join("release/readiness-policy.json"))?)?;
    require(policy.schema == "docsight.readiness-policy/v1" && policy.required_reviews.iter().map(String::as_str).eq(REVIEWS), "INVALID_READINESS_POLICY", "Readiness policy must preserve every review category")?;
    require((5..=999).contains(&policy.minimum_beta_participants) && (2..=100_000).contains(&policy.minimum_corpus_documents) && (1..=50_000).contains(&policy.minimum_real_documents_per_format), "INVALID_READINESS_POLICY", "Readiness thresholds are outside their permitted limits")?; Ok(policy)
}

pub fn identity(value: &Value, schema: &str, revision: &str, version: &str) -> Result<()> {
    require(value.get("schema").and_then(Value::as_str) == Some(schema) && value.get("revision").and_then(Value::as_str) == Some(revision) && value.get("version").and_then(Value::as_str) == Some(version), "EVIDENCE_CANDIDATE_MISMATCH", "Evidence does not describe the exact candidate and schema")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion { pub name: String, pub passed: bool, #[serde(deserialize_with = "required_option")] pub error_code: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report { pub schema: String, pub version: String, pub revision: String, pub policy_sha256: String, pub criteria: Vec<Criterion>, pub ready_for_v1: bool, pub scope: String, }

fn record<T>(criteria: &mut Vec<Criterion>, name: &str, result: Result<T>) -> Option<T> {
    match result { Ok(value) => { criteria.push(Criterion { name: name.into(), passed: true, error_code: None }); Some(value) } Err(error) => { criteria.push(Criterion { name: name.into(), passed: false, error_code: Some(error.code.into()) }); None } }
}

pub fn assess(directory: &Path, revision: &str, root: &Path) -> Result<Report> {
    checked_revision(revision)?; let version = workspace_version(); let policy = load_policy(root)?; let mut criteria = Vec::new();
    let _recorded = record(&mut criteria, CRITERIA[0], evidence::validate_workspace(directory, revision, version));
    let packages = record(&mut criteria, CRITERIA[1], evidence::package_evidence(directory, revision, version)).unwrap_or_default();
    let _recorded = record(&mut criteria, CRITERIA[2], campaigns::beta_evidence(directory, root, &packages, revision, version, policy.minimum_beta_participants));
    let cases = record(&mut criteria, CRITERIA[3], campaigns::corpus_evidence(directory, &packages, revision, version, &policy)).unwrap_or_default();
    let _recorded = record(&mut criteria, CRITERIA[4], reviews::review_evidence(directory, revision, version, root));
    let _recorded = record(&mut criteria, CRITERIA[5], reviews::regression_evidence(directory, &cases, root, revision));
    let _recorded = record(&mut criteria, CRITERIA[6], reviews::known_gaps(root));
    Ok(Report { schema: "docsight.readiness/v1".into(), version: version.into(), revision: revision.into(), policy_sha256: sha256_file(&root.join("release/readiness-policy.json"))?, ready_for_v1: criteria.len() == CRITERIA.len() && criteria.iter().all(|criterion| criterion.passed), criteria, scope: "artifact consistency plus explicitly recorded human reviews; not independent certification".into() })
}
