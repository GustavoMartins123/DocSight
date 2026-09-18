use super::classes::Taxonomy;
use super::engine::{DiffSummary, EngineSession, Observation, render_covers_page, signal_values};
use super::ground_truth::GroundTruth;
use crate::corpus::{Case, Manifest, load_manifest};
use crate::tooling::common::*;
use crate::tooling::process::Runner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const QUALITY_REPORT_SCHEMA: &str = "docsight.quality-report/v1";
pub const METRICS: [&str; 11] = [
    "determinism",
    "classification",
    "rejection",
    "render_geometry",
    "diff_identity",
    "structure",
    "text",
    "geometry",
    "diagnostics",
    "render",
    "diff",
];
pub const STATUSES: [&str; 4] = ["passed", "failed", "not_measured", "not_applicable"];
pub const BASES: [&str; 5] = [
    "engine-invariant",
    "corpus-expectation",
    "reviewed-ground-truth",
    "unreviewed-proposal",
    "none",
];
pub const EXTRAPOLATION: &str = "none: results describe only the listed documents within their declared classes and are not a claim about other documents of the same format";
const GEOMETRY_TOLERANCE_PT: f64 = 0.01;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetricOutcome {
    pub status: String,
    pub basis: String,
    pub detail: Vec<String>,
}

impl MetricOutcome {
    fn new(passed: bool, basis: &str, detail: Vec<String>) -> Self {
        Self {
            status: if passed { "passed" } else { "failed" }.to_owned(),
            basis: basis.to_owned(),
            detail,
        }
    }

    fn not(status: &str, basis: &str) -> Self {
        Self {
            status: status.to_owned(),
            basis: basis.to_owned(),
            detail: Vec::new(),
        }
    }

    /// Failures against engine invariants, declared corpus expectations or reviewed ground
    /// truth fail the measurement; drift from an unreviewed proposal is reported only.
    pub fn blocking_failure(&self) -> bool {
        self.status == "failed" && self.basis != "unreviewed-proposal"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentResult {
    pub document_sha256: String,
    pub class: String,
    pub format: String,
    pub origin: String,
    pub cases: Vec<String>,
    pub ground_truth: String,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<String>,
    pub metrics: BTreeMap<String, MetricOutcome>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Tally {
    pub passed: u64,
    pub failed: u64,
    pub not_measured: u64,
    pub not_applicable: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClassSummary {
    pub class: String,
    pub format: String,
    pub complexity: String,
    pub documents: u64,
    pub synthetic_documents: u64,
    pub consented_real_documents: u64,
    pub reviewed_ground_truth: u64,
    pub unreviewed_proposals: u64,
    pub failed_documents: u64,
    pub evidence: String,
    pub metrics: BTreeMap<String, BTreeMap<String, Tally>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QualityReport {
    pub schema: String,
    pub version: String,
    pub revision: String,
    pub target: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub classes_sha256: String,
    pub ground_truth_records: u64,
    pub reviewed_ground_truth_records: u64,
    pub extrapolation: String,
    pub documents: Vec<DocumentResult>,
    pub classes: Vec<ClassSummary>,
    pub passed: bool,
}

struct Unit<'a> {
    cases: Vec<&'a Case>,
    references: BTreeMap<String, PathBuf>,
}

pub struct MeasureInputs<'a> {
    pub archive: &'a Path,
    pub manifest: &'a Path,
    pub root: &'a Path,
    pub taxonomy: &'a Taxonomy,
    pub classes_sha256: String,
    pub register: &'a BTreeMap<String, GroundTruth>,
}

pub fn measure_with<R: Runner>(
    inputs: &MeasureInputs<'_>,
    runner: &mut R,
) -> Result<QualityReport> {
    let manifest_sha256 = sha256_file(inputs.manifest)?;
    let manifest: Manifest = load_manifest(inputs.manifest, Some(inputs.root), inputs.taxonomy)?;
    let units = group(&manifest, inputs.root)?;
    for record in inputs.register.values() {
        if let Some(unit) = units.get(&record.document_sha256) {
            require(
                unit.cases[0].class == record.class,
                "QUALITY_CLASS_CONFLICT",
                "Ground truth and corpus declare different classes for one document",
            )?;
        }
    }
    let mut session = EngineSession::open(inputs.archive, runner)?;
    let mut documents = Vec::new();
    for (sha256, unit) in &units {
        let record = inputs.register.get(sha256);
        documents.push(measure_unit(&mut session, inputs, sha256, unit, record));
    }
    require(
        sha256_file(inputs.manifest)? == manifest_sha256,
        "QUALITY_EVIDENCE_CHANGED",
        "The corpus manifest changed while it was measured",
    )?;
    let classes = summarize(inputs.taxonomy, &documents);
    let passed = documents.iter().all(|document| {
        document.error_code.is_none()
            && document
                .metrics
                .values()
                .all(|metric| !metric.blocking_failure())
    });
    Ok(QualityReport {
        schema: QUALITY_REPORT_SCHEMA.to_owned(),
        version: session.version.clone(),
        revision: session.revision.clone(),
        target: session.target.clone(),
        archive_sha256: session.archive_sha256.clone(),
        manifest_sha256,
        classes_sha256: inputs.classes_sha256.clone(),
        ground_truth_records: inputs.register.len() as u64,
        reviewed_ground_truth_records: inputs
            .register
            .values()
            .filter(|record| record.reviewed())
            .count() as u64,
        extrapolation: EXTRAPOLATION.to_owned(),
        documents,
        classes,
        passed,
    })
}

fn group<'a>(manifest: &'a Manifest, root: &Path) -> Result<BTreeMap<String, Unit<'a>>> {
    let mut units: BTreeMap<String, Unit<'a>> = BTreeMap::new();
    for case in &manifest.cases {
        let unit = units.entry(case.sha256.clone()).or_insert_with(|| Unit {
            cases: Vec::new(),
            references: BTreeMap::new(),
        });
        if let Some(first) = unit.cases.first() {
            require(
                first.class == case.class
                    && first.format == case.format
                    && first.origin == case.origin,
                "QUALITY_CLASS_CONFLICT",
                "Every case of one document declares the same class, format and origin",
            )?;
        }
        if let Some(reference) = &case.reference {
            unit.references.insert(
                reference.sha256.clone(),
                contained_file(root, &reference.file)?,
            );
        }
        unit.cases.push(case);
    }
    Ok(units)
}

fn measure_unit<R: Runner>(
    session: &mut EngineSession<'_, R>,
    inputs: &MeasureInputs<'_>,
    sha256: &str,
    unit: &Unit<'_>,
    record: Option<&GroundTruth>,
) -> DocumentResult {
    let first = unit.cases[0];
    let mut result = DocumentResult {
        document_sha256: sha256.to_owned(),
        class: first.class.clone(),
        format: first.format.clone(),
        origin: first.origin.clone(),
        cases: unit.cases.iter().map(|case| case.id.clone()).collect(),
        ground_truth: match record {
            Some(record) if record.reviewed() => "reviewed",
            Some(_) => "unreviewed",
            None => "absent",
        }
        .to_owned(),
        error_code: None,
        metrics: BTreeMap::new(),
    };
    let measured = (|| -> Result<BTreeMap<String, MetricOutcome>> {
        let document = contained_file(inputs.root, &first.file)?;
        let class = inputs.taxonomy.get(&first.class)?;
        let metrics = if class.expects_failure {
            measure_rejection(session, &document, &unit.cases)?
        } else {
            measure_document(session, &document, class, unit, record)?
        };
        require(
            sha256_file(&document)? == sha256,
            "QUALITY_EVIDENCE_CHANGED",
            "A corpus document changed while it was measured",
        )?;
        Ok(metrics)
    })();
    match measured {
        Ok(metrics) => result.metrics = metrics,
        Err(error) => {
            result.error_code = Some(error.code.to_owned());
            result.metrics = METRICS
                .iter()
                .map(|metric| {
                    (
                        (*metric).to_owned(),
                        MetricOutcome::not("not_measured", "none"),
                    )
                })
                .collect();
        }
    }
    result
}

fn measure_rejection<R: Runner>(
    session: &mut EngineSession<'_, R>,
    document: &Path,
    cases: &[&Case],
) -> Result<BTreeMap<String, MetricOutcome>> {
    let mut metrics = not_applicable();
    let mut outputs = BTreeSet::new();
    let mut detail = Vec::new();
    for case in cases {
        for _ in 0..2 {
            let result = session.run(&["inspect".into(), document.as_os_str().to_owned()])?;
            outputs.insert((result.stdout.clone(), result.stderr.clone()));
            let codes = error_codes(&result.stderr);
            if result.returncode != i64::from(case.expected.exit_code) {
                detail.push(format!("{}:exit-code-{}", case.id, result.returncode));
            }
            for code in &case.expected.diagnostic_codes {
                if !codes.contains(code) {
                    detail.push(format!("{}:missing-{code}", case.id));
                }
            }
            if !result.stdout.is_empty() {
                detail.push(format!("{}:stdout-on-error", case.id));
            }
        }
    }
    detail.sort();
    detail.dedup();
    metrics.insert(
        "rejection".to_owned(),
        MetricOutcome::new(detail.is_empty(), "corpus-expectation", detail),
    );
    metrics.insert(
        "determinism".to_owned(),
        MetricOutcome::new(outputs.len() == 1, "engine-invariant", Vec::new()),
    );
    Ok(metrics)
}

fn measure_document<R: Runner>(
    session: &mut EngineSession<'_, R>,
    document: &Path,
    class: &super::classes::DocumentClass,
    unit: &Unit<'_>,
    record: Option<&GroundTruth>,
) -> Result<BTreeMap<String, MetricOutcome>> {
    let mut metrics = BTreeMap::new();
    let first = session.observe(document)?;
    let second = session.observe(document)?;
    let identity = session.diff(document, document)?;
    let mut diffs: BTreeMap<String, DiffSummary> = BTreeMap::new();
    let mut diff_repeats_match = true;
    for (reference_sha256, reference) in &unit.references {
        let summary = session.diff(document, reference)?;
        diff_repeats_match &= session.diff(document, reference)? == summary;
        diffs.insert(reference_sha256.clone(), summary);
    }

    metrics.insert(
        "determinism".to_owned(),
        MetricOutcome::new(
            first == second && diff_repeats_match,
            "engine-invariant",
            differing_sections(&first, &second),
        ),
    );
    let violations = class.violations(&signal_values(&first.structure));
    metrics.insert(
        "classification".to_owned(),
        MetricOutcome::new(violations.is_empty(), "engine-invariant", violations),
    );
    metrics.insert(
        "rejection".to_owned(),
        MetricOutcome::not("not_applicable", "none"),
    );
    metrics.insert(
        "render_geometry".to_owned(),
        MetricOutcome::new(
            render_covers_page(&first.render, &first.geometry),
            "engine-invariant",
            Vec::new(),
        ),
    );
    metrics.insert(
        "diff_identity".to_owned(),
        MetricOutcome::new(
            identity.semantic_changes == 0
                && identity.layout_changed_pages == 0
                && identity.pages_before == identity.pages_after,
            "engine-invariant",
            Vec::new(),
        ),
    );

    let Some(record) = record else {
        for metric in [
            "structure",
            "text",
            "geometry",
            "diagnostics",
            "render",
            "diff",
        ] {
            metrics.insert(
                metric.to_owned(),
                MetricOutcome::not("not_measured", "none"),
            );
        }
        return Ok(metrics);
    };
    let basis = if record.reviewed() {
        "reviewed-ground-truth"
    } else {
        "unreviewed-proposal"
    };
    let expected = &record.expected;
    metrics.insert(
        "structure".to_owned(),
        MetricOutcome::new(
            first.structure == expected.structure,
            basis,
            structure_detail(&first, expected),
        ),
    );
    let mut text_detail = Vec::new();
    if first.text.blocks != expected.text.blocks {
        text_detail.push(format!(
            "blocks:{}->{}",
            expected.text.blocks, first.text.blocks
        ));
    }
    if first.text.characters != expected.text.characters {
        text_detail.push(format!(
            "characters:{}->{}",
            expected.text.characters, first.text.characters
        ));
    }
    if first.text.sha256 != expected.text.sha256 {
        text_detail.push("content:changed".to_owned());
    }
    metrics.insert(
        "text".to_owned(),
        MetricOutcome::new(first.text == expected.text, basis, text_detail),
    );
    let geometry_matches = first.geometry.page == expected.geometry.page
        && (first.geometry.width_pt - expected.geometry.width_pt).abs() <= GEOMETRY_TOLERANCE_PT
        && (first.geometry.height_pt - expected.geometry.height_pt).abs() <= GEOMETRY_TOLERANCE_PT
        && first.geometry.spans == expected.geometry.spans;
    metrics.insert(
        "geometry".to_owned(),
        MetricOutcome::new(geometry_matches, basis, Vec::new()),
    );
    let observed: BTreeSet<_> = first.diagnostics.iter().collect();
    let declared: BTreeSet<_> = expected.diagnostics.iter().collect();
    let mut diagnostic_detail: Vec<String> = declared
        .difference(&observed)
        .map(|code| format!("missing:{code}"))
        .chain(
            observed
                .difference(&declared)
                .map(|code| format!("unexpected:{code}")),
        )
        .collect();
    diagnostic_detail.sort();
    metrics.insert(
        "diagnostics".to_owned(),
        MetricOutcome::new(diagnostic_detail.is_empty(), basis, diagnostic_detail),
    );
    metrics.insert(
        "render".to_owned(),
        MetricOutcome::new(first.render == expected.render, basis, Vec::new()),
    );
    let expectations: BTreeMap<_, _> = record
        .diffs
        .iter()
        .map(|diff| (diff.reference_sha256.as_str(), &diff.summary))
        .collect();
    let mut diff_detail = Vec::new();
    let mut compared = false;
    for (reference_sha256, summary) in &diffs {
        if let Some(expected) = expectations.get(reference_sha256.as_str()) {
            compared = true;
            if *expected != summary {
                diff_detail.push(format!("reference:{reference_sha256}"));
            }
        }
    }
    metrics.insert(
        "diff".to_owned(),
        if compared {
            MetricOutcome::new(diff_detail.is_empty(), basis, diff_detail)
        } else {
            MetricOutcome::not("not_measured", "none")
        },
    );
    Ok(metrics)
}

fn not_applicable() -> BTreeMap<String, MetricOutcome> {
    METRICS
        .iter()
        .map(|metric| {
            (
                (*metric).to_owned(),
                MetricOutcome::not("not_applicable", "none"),
            )
        })
        .collect()
}

fn error_codes(stderr: &[u8]) -> BTreeSet<String> {
    parse_json(stderr)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .into_iter()
        .collect()
}

fn differing_sections(first: &Observation, second: &Observation) -> Vec<String> {
    let mut sections = Vec::new();
    if first.structure != second.structure {
        sections.push("structure".to_owned());
    }
    if first.text != second.text {
        sections.push("text".to_owned());
    }
    if first.geometry != second.geometry {
        sections.push("geometry".to_owned());
    }
    if first.diagnostics != second.diagnostics {
        sections.push("diagnostics".to_owned());
    }
    if first.render != second.render {
        sections.push("render".to_owned());
    }
    sections
}

fn structure_detail(observed: &Observation, expected: &Observation) -> Vec<String> {
    let pairs = [
        ("pages", observed.structure.pages, expected.structure.pages),
        (
            "paragraphs",
            observed.structure.paragraphs,
            expected.structure.paragraphs,
        ),
        (
            "headings",
            observed.structure.headings,
            expected.structure.headings,
        ),
        (
            "tables",
            observed.structure.tables,
            expected.structure.tables,
        ),
        (
            "figures",
            observed.structure.figures,
            expected.structure.figures,
        ),
    ];
    let mut detail: Vec<String> = pairs
        .iter()
        .filter(|(_, observed, expected)| observed != expected)
        .map(|(name, observed, expected)| format!("{name}:{expected}->{observed}"))
        .collect();
    if observed.structure.blocks_by_kind != expected.structure.blocks_by_kind {
        detail.push("blocks_by_kind".to_owned());
    }
    detail
}

fn summarize(taxonomy: &Taxonomy, documents: &[DocumentResult]) -> Vec<ClassSummary> {
    taxonomy
        .classes
        .iter()
        .map(|class| {
            let members: Vec<_> = documents
                .iter()
                .filter(|document| document.class == class.id)
                .collect();
            let count = |predicate: &dyn Fn(&DocumentResult) -> bool| {
                members
                    .iter()
                    .filter(|document| predicate(document))
                    .count() as u64
            };
            let reviewed = count(&|document| document.ground_truth == "reviewed");
            let mut metrics: BTreeMap<String, BTreeMap<String, Tally>> = BTreeMap::new();
            for document in &members {
                for (name, outcome) in &document.metrics {
                    let tally = metrics
                        .entry(name.clone())
                        .or_default()
                        .entry(outcome.basis.clone())
                        .or_default();
                    match outcome.status.as_str() {
                        "passed" => tally.passed += 1,
                        "failed" => tally.failed += 1,
                        "not_measured" => tally.not_measured += 1,
                        _ => tally.not_applicable += 1,
                    }
                }
            }
            let documents = members.len() as u64;
            let evidence = if documents == 0 {
                "no-documents"
            } else if class.expects_failure {
                "corpus-expectation"
            } else if reviewed == documents {
                "reviewed-ground-truth"
            } else if reviewed > 0 {
                "partially-reviewed-ground-truth"
            } else {
                "engine-consistency-only"
            };
            ClassSummary {
                class: class.id.clone(),
                format: class.format.clone(),
                complexity: class.complexity.clone(),
                documents,
                synthetic_documents: count(&|document| document.origin == "synthetic"),
                consented_real_documents: count(&|document| document.origin == "consented-real"),
                reviewed_ground_truth: reviewed,
                unreviewed_proposals: count(&|document| document.ground_truth == "unreviewed"),
                failed_documents: count(&|document| {
                    document.error_code.is_some()
                        || document
                            .metrics
                            .values()
                            .any(MetricOutcome::blocking_failure)
                }),
                evidence: evidence.to_owned(),
                metrics,
            }
        })
        .collect()
}

pub fn validate_report(report: &QualityReport) -> Result<()> {
    require(
        report.schema == QUALITY_REPORT_SCHEMA && report.extrapolation == EXTRAPOLATION,
        "INVALID_QUALITY_REPORT",
        "A quality report uses the versioned schema and its fixed extrapolation statement",
    )?;
    for digest in [
        &report.archive_sha256,
        &report.manifest_sha256,
        &report.classes_sha256,
    ] {
        checked_digest(digest)?;
    }
    checked_revision(&report.revision)?;
    let mut seen = BTreeSet::new();
    for document in &report.documents {
        checked_digest(&document.document_sha256)?;
        require(
            seen.insert(&document.document_sha256)
                && ["reviewed", "unreviewed", "absent"].contains(&document.ground_truth.as_str())
                && document.metrics.len() == METRICS.len()
                && METRICS
                    .iter()
                    .all(|metric| document.metrics.contains_key(*metric))
                && document.metrics.values().all(|metric| {
                    STATUSES.contains(&metric.status.as_str())
                        && BASES.contains(&metric.basis.as_str())
                }),
            "INVALID_QUALITY_REPORT",
            "Every measured document reports each metric once with a known status and basis",
        )?;
    }
    let passed = report.documents.iter().all(|document| {
        document.error_code.is_none()
            && document
                .metrics
                .values()
                .all(|metric| !metric.blocking_failure())
    });
    require(
        passed == report.passed,
        "INVALID_QUALITY_REPORT",
        "The report verdict must follow from its document results",
    )
}
