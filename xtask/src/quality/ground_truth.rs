use super::classes::Taxonomy;
use super::engine::{DiffSummary, EngineSession, Observation};
use crate::corpus::Input;
use crate::tooling::common::*;
use crate::tooling::process::Runner;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const GROUND_TRUTH_SCHEMA: &str = "docsight.ground-truth/v1";
pub const REVIEW_STATUSES: [&str; 2] = ["unreviewed", "reviewed"];
const MAX_RECORDS: usize = 10_000;
const MIN_REVIEW_NOTE_BYTES: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiffExpectation {
    pub reference_sha256: String,
    pub summary: DiffSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub engine_version: String,
    pub revision: String,
    pub archive_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub status: String,
    #[serde(deserialize_with = "required_option")]
    pub reviewer: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub evidence: Option<Input>,
}

/// Expected facts about one document. A record proposed from engine output stays
/// `unreviewed` until a person checks it against the document and records the review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GroundTruth {
    pub schema: String,
    pub document_sha256: String,
    pub format: String,
    pub class: String,
    pub expected: Observation,
    pub diffs: Vec<DiffExpectation>,
    pub proposed_by: Proposal,
    pub review: Review,
}

impl GroundTruth {
    pub fn reviewed(&self) -> bool {
        self.review.status == "reviewed"
    }
}

pub fn validate_record(
    record: GroundTruth,
    taxonomy: &Taxonomy,
    register: Option<&Path>,
) -> Result<GroundTruth> {
    require(
        record.schema == GROUND_TRUTH_SCHEMA,
        "INVALID_GROUND_TRUTH",
        "Ground truth records must use the versioned schema",
    )?;
    checked_digest(&record.document_sha256)?;
    let class = taxonomy.get(&record.class)?;
    require(
        class.format == record.format && !class.expects_failure,
        "INVALID_GROUND_TRUTH",
        "Ground truth describes a parsed document of the declared class format",
    )?;
    checked_version(&record.proposed_by.engine_version)?;
    checked_revision(&record.proposed_by.revision)?;
    checked_digest(&record.proposed_by.archive_sha256)?;
    checked_digest(&record.expected.text.sha256)?;
    checked_digest(&record.expected.render.png_sha256)?;
    require(
        record
            .expected
            .diagnostics
            .windows(2)
            .all(|pair| pair[0] < pair[1])
            && record.expected.diagnostics.iter().all(|code| is_code(code)),
        "INVALID_GROUND_TRUTH",
        "Expected diagnostics must be sorted unique public codes",
    )?;
    require(
        record.expected.geometry.width_pt.is_finite()
            && record.expected.geometry.height_pt.is_finite()
            && record.expected.geometry.width_pt > 0.0
            && record.expected.geometry.height_pt > 0.0,
        "INVALID_GROUND_TRUTH",
        "Expected page geometry must be positive and finite",
    )?;
    let mut references = std::collections::BTreeSet::new();
    for diff in &record.diffs {
        checked_digest(&diff.reference_sha256)?;
        require(
            references.insert(diff.reference_sha256.clone()),
            "INVALID_GROUND_TRUTH",
            "A reference document appears in more than one diff expectation",
        )?;
    }
    let review = &record.review;
    require(
        REVIEW_STATUSES.contains(&review.status.as_str()),
        "INVALID_GROUND_TRUTH",
        "Review status must be unreviewed or reviewed",
    )?;
    if record.reviewed() {
        let reviewer = review.reviewer.as_deref().unwrap_or_default();
        require(
            (3..=80).contains(&reviewer.len())
                && reviewer
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "UNREVIEWED_GROUND_TRUTH",
            "A reviewed record names a stable reviewer identifier",
        )?;
        let evidence = review.evidence.as_ref().ok_or_else(|| {
            ToolError::new(
                "UNREVIEWED_GROUND_TRUTH",
                "A reviewed record binds the note that documents the review",
            )
        })?;
        safe_member(&evidence.file)?;
        checked_digest(&evidence.sha256)?;
        if let Some(register) = register {
            let note = contained_file(register, &evidence.file)?;
            let bytes = read_bytes(&note, 1_048_576)?;
            require(
                digest(&bytes) == evidence.sha256 && bytes.len() as u64 >= MIN_REVIEW_NOTE_BYTES,
                "UNREVIEWED_GROUND_TRUTH",
                "The review note is missing, changed or too short to document a review",
            )?;
        }
    } else {
        require(
            review.reviewer.is_none() && review.evidence.is_none(),
            "INVALID_GROUND_TRUTH",
            "An unreviewed record carries no reviewer or review evidence",
        )?;
    }
    Ok(record)
}

/// Loads every `*.json` record of a register directory, keyed by document digest.
pub fn load_register(
    register: &Path,
    taxonomy: &Taxonomy,
) -> Result<BTreeMap<String, GroundTruth>> {
    let mut records = BTreeMap::new();
    for path in flat_files(register, "json", MAX_RECORDS)? {
        let record: GroundTruth = decode(read_json(&path)?)?;
        let record = validate_record(record, taxonomy, Some(register))?;
        require(
            path.file_stem().and_then(|stem| stem.to_str())
                == Some(record.document_sha256.as_str()),
            "INVALID_GROUND_TRUTH",
            "A record file is named after the digest of the document it describes",
        )?;
        require(
            !records.contains_key(&record.document_sha256),
            "INVALID_GROUND_TRUTH",
            "A document has more than one ground truth record",
        )?;
        records.insert(record.document_sha256.clone(), record);
    }
    Ok(records)
}

/// Proposes a record from what the engine reports. The proposal is never marked reviewed.
pub fn prepare_with<R: Runner>(
    archive: &Path,
    document: &Path,
    references: &[&Path],
    class: &str,
    taxonomy: &Taxonomy,
    runner: &mut R,
) -> Result<GroundTruth> {
    let declared = taxonomy.get(class)?;
    require(
        !declared.expects_failure,
        "INVALID_GROUND_TRUTH",
        "Ground truth describes parsed documents, not rejected inputs",
    )?;
    let document = &document.canonicalize()?;
    let document_sha256 = sha256_file(document)?;
    let mut session = EngineSession::open(archive, runner)?;
    let expected = session.observe(document)?;
    let mut diffs = Vec::new();
    for reference in references {
        let reference = reference.canonicalize()?;
        diffs.push(DiffExpectation {
            reference_sha256: sha256_file(&reference)?,
            summary: session.diff(document, &reference)?,
        });
    }
    diffs.sort_by(|left, right| left.reference_sha256.cmp(&right.reference_sha256));
    require(
        sha256_file(document)? == document_sha256,
        "QUALITY_EVIDENCE_CHANGED",
        "The document changed while its ground truth was proposed",
    )?;
    let record = GroundTruth {
        schema: GROUND_TRUTH_SCHEMA.to_owned(),
        document_sha256,
        format: declared.format.clone(),
        class: class.to_owned(),
        expected,
        diffs,
        proposed_by: Proposal {
            engine_version: session.version.clone(),
            revision: session.revision.clone(),
            archive_sha256: session.archive_sha256.clone(),
        },
        review: Review {
            status: "unreviewed".to_owned(),
            reviewer: None,
            evidence: None,
        },
    };
    validate_record(record, taxonomy, None)
}
