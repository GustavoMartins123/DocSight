#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::BTreeMap;
use support::*;
use xtask::corpus::load_manifest;
use xtask::quality::{
    ClassSummary, DocumentResult, EXTRAPOLATION, GroundTruth, MeasureInputs, QualityReport, Review,
    Taxonomy, compare, load_register, measure_with, prepare_with, validate_record, validate_report,
    validate_taxonomy,
};
use xtask::tooling::common::{digest, workspace_root};
use xtask::tooling::process::ProcessLimits;

fn class_document(overrides: serde_json::Value) -> TestResult<Taxonomy> {
    let mut class = json!({
        "id": "docx-sample",
        "format": "docx",
        "complexity": "simple",
        "expects_failure": false,
        "description": "Sample class.",
        "signals": {"paragraphs": {"min": 1}},
    });
    if let (Some(target), Some(source)) = (class.as_object_mut(), overrides.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(serde_json::from_value(json!({
        "schema": "docsight.document-classes/v1",
        "note": "Test taxonomy.",
        "classes": [class],
    }))?)
}

#[test]
fn repository_taxonomy_classifies_every_corpus_case() -> TestResult {
    let root = workspace_root();
    let taxonomy = taxonomy();
    assert!(taxonomy.classes.len() >= 10);
    for format in ["docx", "pdf", "invalid"] {
        assert!(
            taxonomy.classes.iter().any(|class| class.format == format),
            "no class describes {format}"
        );
    }
    let manifest = load_manifest(&root.join("release/corpus.json"), Some(&root), &taxonomy)?;
    for case in &manifest.cases {
        let class = taxonomy.get(&case.class)?;
        assert_eq!(class.format, case.format, "{}", case.id);
        assert_eq!(
            class.expects_failure,
            case.expected.exit_code != 0,
            "{}",
            case.id
        );
    }
    Ok(())
}

#[test]
fn invalid_taxonomies_fail_with_a_specific_code() -> TestResult {
    let mut variants: Vec<Taxonomy> = Vec::new();

    let mut taxonomy = class_document(json!({}))?;
    taxonomy.schema = "docsight.document-classes/v2".into();
    variants.push(taxonomy);

    let mut taxonomy = class_document(json!({}))?;
    taxonomy.classes.push(taxonomy.classes[0].clone());
    variants.push(taxonomy);

    variants.push(class_document(json!({"id": "Upper"}))?);
    variants.push(class_document(json!({"format": "xlsx"}))?);
    variants.push(class_document(json!({"complexity": "extreme"}))?);
    variants.push(class_document(json!({"expects_failure": true}))?);
    variants.push(class_document(json!({"signals": {}}))?);
    variants.push(class_document(
        json!({"complexity": "adversarial", "expects_failure": true}),
    )?);
    variants.push(class_document(json!({"signals": {"words": {"min": 1}}}))?);
    variants.push(class_document(json!({"signals": {"pages": {}}}))?);
    variants.push(class_document(
        json!({"signals": {"pages": {"min": 3, "max": 2}}}),
    )?);
    variants.push(class_document(json!({"description": ""}))?);

    for taxonomy in variants {
        assert_eq!(
            code(validate_taxonomy(taxonomy)),
            Some("INVALID_DOCUMENT_CLASSES")
        );
    }
    assert!(validate_taxonomy(class_document(json!({}))?).is_ok());
    assert!(
        validate_taxonomy(class_document(json!({
            "format": "invalid",
            "complexity": "adversarial",
            "expects_failure": true,
            "signals": {},
        }))?)
        .is_ok()
    );
    Ok(())
}

#[test]
fn class_signals_expose_a_mislabelled_document() -> TestResult {
    let taxonomy = taxonomy();
    let tabular = taxonomy.get("docx-tabular")?;
    let text = taxonomy.get("docx-text")?;
    let measured = BTreeMap::from([
        ("pages".to_owned(), 2),
        ("paragraphs".to_owned(), 30),
        ("headings".to_owned(), 0),
        ("tables".to_owned(), 3),
        ("figures".to_owned(), 0),
    ]);
    assert!(tabular.violations(&measured).is_empty());
    assert_eq!(text.violations(&measured), vec!["tables:above-maximum"]);

    let no_tables = BTreeMap::from([("tables".to_owned(), 0)]);
    assert_eq!(tabular.violations(&no_tables), vec!["tables:below-minimum"]);
    assert_eq!(
        tabular.violations(&BTreeMap::new()),
        vec!["tables:unmeasured"]
    );
    Ok(())
}

struct Bench {
    fixture: Fixture,
    archive: std::path::PathBuf,
    manifest: std::path::PathBuf,
    register: std::path::PathBuf,
}

fn case(
    id: &str,
    file: &str,
    data: &[u8],
    format: &str,
    class: &str,
    operation: &str,
) -> xtask::corpus::Case {
    xtask::corpus::Case {
        id: id.into(),
        file: file.into(),
        sha256: digest(data),
        origin: "synthetic".into(),
        format: format.into(),
        class: class.into(),
        operation: operation.into(),
        reference: None,
        expected: xtask::corpus::Expectation {
            exit_code: 0,
            diagnostic_codes: Vec::new(),
            pointer_equals: BTreeMap::new(),
            repeat: 1,
        },
    }
}

impl Bench {
    const DOC: &'static [u8] = b"docx under measurement";
    const OTHER: &'static [u8] = b"previous docx version";
    const TABLE: &'static [u8] = b"pdf under measurement";
    const BAD: &'static [u8] = b"not a document";

    fn new() -> TestResult<Self> {
        let fixture = Fixture::new()?;
        for (name, data) in [
            ("doc.docx", Self::DOC),
            ("other.docx", Self::OTHER),
            ("table.pdf", Self::TABLE),
            ("bad.bin", Self::BAD),
        ] {
            std::fs::write(fixture.root.join(name), data)?;
        }
        let archive = fixture.package(xtask::release::native_target()?, "dist")?;
        let mut diff = case(
            "doc-diff",
            "doc.docx",
            Self::DOC,
            "docx",
            "docx-tabular",
            "diff",
        );
        diff.reference = Some(xtask::corpus::Input {
            file: "other.docx".into(),
            sha256: digest(Self::OTHER),
        });
        let mut bad = case(
            "bad-inspect",
            "bad.bin",
            Self::BAD,
            "invalid",
            "unsupported-format",
            "inspect",
        );
        bad.expected.exit_code = 10;
        bad.expected.diagnostic_codes = vec!["UNSUPPORTED_FORMAT".into()];
        let manifest = fixture.root.join("corpus.json");
        save(
            &manifest,
            &xtask::corpus::Manifest {
                schema: "docsight.corpus/v2".into(),
                cases: vec![
                    case(
                        "doc-inspect",
                        "doc.docx",
                        Self::DOC,
                        "docx",
                        "docx-tabular",
                        "inspect",
                    ),
                    diff,
                    case(
                        "table-inspect",
                        "table.pdf",
                        Self::TABLE,
                        "pdf",
                        "pdf-tabular",
                        "inspect",
                    ),
                    bad,
                ],
            },
        )?;
        let register = fixture.root.join("ground-truth");
        std::fs::create_dir(&register)?;
        Ok(Self {
            fixture,
            archive,
            manifest,
            register,
        })
    }

    fn measure(&self, engine: &mut FakeEngine) -> TestResult<QualityReport> {
        Ok(self.try_measure(engine)?)
    }

    fn try_measure(
        &self,
        engine: &mut FakeEngine,
    ) -> xtask::tooling::common::Result<QualityReport> {
        let taxonomy = taxonomy();
        let register = load_register(&self.register, &taxonomy)?;
        measure_with(
            &MeasureInputs {
                archive: &self.archive,
                manifest: &self.manifest,
                root: &self.fixture.root,
                taxonomy: &taxonomy,
                classes_sha256: digest(b"classes"),
                register: &register,
            },
            &mut Callback(
                |arguments: &[std::ffi::OsString],
                 _: &std::path::Path,
                 _: &ProcessLimits,
                 _: Option<&BTreeMap<std::ffi::OsString, std::ffi::OsString>>| {
                    engine.respond(arguments)
                },
            ),
        )
    }

    fn propose(
        &self,
        engine: &mut FakeEngine,
        file: &str,
        class: &str,
        references: &[&str],
    ) -> TestResult<GroundTruth> {
        let references: Vec<std::path::PathBuf> = references
            .iter()
            .map(|name| self.fixture.root.join(name))
            .collect();
        let references: Vec<&std::path::Path> =
            references.iter().map(std::path::PathBuf::as_path).collect();
        Ok(prepare_with(
            &self.archive,
            &self.fixture.root.join(file),
            &references,
            class,
            &taxonomy(),
            &mut Callback(
                |arguments: &[std::ffi::OsString],
                 _: &std::path::Path,
                 _: &ProcessLimits,
                 _: Option<&BTreeMap<std::ffi::OsString, std::ffi::OsString>>| {
                    engine.respond(arguments)
                },
            ),
        )?)
    }

    fn store(&self, record: &GroundTruth) -> TestResult {
        save(
            &self
                .register
                .join(format!("{}.json", record.document_sha256)),
            record,
        )
    }

    fn review(&self, record: &mut GroundTruth) -> TestResult {
        let note = format!(
            "# Review of {}\n\nProcedure: compared every expected value against the source document by hand.\nObservations: values match the document.\nLimitations: synthetic unit-test note.\n",
            record.document_sha256
        );
        std::fs::create_dir_all(self.register.join("reviews"))?;
        let file = format!("reviews/{}.md", record.document_sha256);
        std::fs::write(self.register.join(&file), &note)?;
        record.review = Review {
            status: "reviewed".into(),
            reviewer: Some("reviewer-01".into()),
            evidence: Some(xtask::corpus::Input {
                file,
                sha256: digest(note.as_bytes()),
            }),
        };
        Ok(())
    }
}

fn document<'a>(report: &'a QualityReport, data: &[u8]) -> TestResult<&'a DocumentResult> {
    let sha256 = digest(data);
    Ok(report
        .documents
        .iter()
        .find(|document| document.document_sha256 == sha256)
        .ok_or("measured document")?)
}

fn metric(document: &DocumentResult, name: &str) -> TestResult<(String, String)> {
    let outcome = document.metrics.get(name).ok_or("metric")?;
    Ok((outcome.status.clone(), outcome.basis.clone()))
}

fn pair(status: &str, basis: &str) -> (String, String) {
    (status.to_owned(), basis.to_owned())
}

fn class_summary<'a>(report: &'a QualityReport, class: &str) -> TestResult<&'a ClassSummary> {
    Ok(report
        .classes
        .iter()
        .find(|summary| summary.class == class)
        .ok_or("class summary")?)
}

#[test]
fn without_ground_truth_only_engine_consistency_is_measured() -> TestResult {
    let bench = Bench::new()?;
    let report = bench.measure(&mut FakeEngine::default())?;
    assert!(report.passed, "{report:?}");
    assert_eq!(report.extrapolation, EXTRAPOLATION);
    assert_eq!(report.ground_truth_records, 0);
    assert_eq!(report.documents.len(), 3);
    validate_report(&report)?;

    let doc = document(&report, Bench::DOC)?;
    assert_eq!(doc.cases, vec!["doc-inspect", "doc-diff"]);
    assert_eq!(doc.ground_truth, "absent");
    for name in [
        "determinism",
        "classification",
        "render_geometry",
        "diff_identity",
    ] {
        assert_eq!(
            metric(doc, name)?,
            pair("passed", "engine-invariant"),
            "{name}"
        );
    }
    for name in [
        "structure",
        "text",
        "geometry",
        "diagnostics",
        "render",
        "diff",
    ] {
        assert_eq!(metric(doc, name)?, pair("not_measured", "none"), "{name}");
    }
    assert_eq!(metric(doc, "rejection")?, pair("not_applicable", "none"));

    let bad = document(&report, Bench::BAD)?;
    assert_eq!(
        metric(bad, "rejection")?,
        pair("passed", "corpus-expectation")
    );
    assert_eq!(
        metric(bad, "determinism")?,
        pair("passed", "engine-invariant")
    );
    assert_eq!(metric(bad, "structure")?, pair("not_applicable", "none"));

    assert_eq!(report.classes.len(), taxonomy().classes.len());
    let tabular = class_summary(&report, "docx-tabular")?;
    assert_eq!(tabular.evidence, "engine-consistency-only");
    assert_eq!((tabular.documents, tabular.synthetic_documents), (1, 1));
    assert_eq!(tabular.consented_real_documents, 0);
    assert_eq!(
        class_summary(&report, "unsupported-format")?.evidence,
        "corpus-expectation"
    );
    assert_eq!(
        class_summary(&report, "docx-visual")?.evidence,
        "no-documents"
    );
    Ok(())
}

#[test]
fn an_unreviewed_proposal_is_compared_but_never_counts_as_reviewed() -> TestResult {
    let bench = Bench::new()?;
    let mut engine = FakeEngine::default();
    let proposal = bench.propose(&mut engine, "doc.docx", "docx-tabular", &["other.docx"])?;
    assert_eq!(proposal.review.status, "unreviewed");
    assert!(proposal.review.reviewer.is_none() && proposal.review.evidence.is_none());
    assert_eq!(proposal.diffs.len(), 1);
    assert_eq!(proposal.diffs[0].summary.semantic_changes, 3);
    bench.store(&proposal)?;

    let report = bench.measure(&mut engine)?;
    assert!(report.passed);
    assert_eq!(
        (
            report.ground_truth_records,
            report.reviewed_ground_truth_records
        ),
        (1, 0)
    );
    let doc = document(&report, Bench::DOC)?;
    assert_eq!(doc.ground_truth, "unreviewed");
    for name in [
        "structure",
        "text",
        "geometry",
        "diagnostics",
        "render",
        "diff",
    ] {
        assert_eq!(
            metric(doc, name)?,
            pair("passed", "unreviewed-proposal"),
            "{name}"
        );
    }
    let tabular = class_summary(&report, "docx-tabular")?;
    assert_eq!(tabular.evidence, "engine-consistency-only");
    assert_eq!(
        (tabular.reviewed_ground_truth, tabular.unreviewed_proposals),
        (0, 1)
    );

    let mut changed = FakeEngine {
        text: "Revised figures".into(),
        tables: 3,
        ..FakeEngine::default()
    };
    let drifted = bench.measure(&mut changed)?;
    assert!(
        drifted.passed,
        "drift from an unreviewed proposal is reported only"
    );
    let doc = document(&drifted, Bench::DOC)?;
    assert_eq!(metric(doc, "text")?, pair("failed", "unreviewed-proposal"));
    assert_eq!(
        metric(doc, "structure")?,
        pair("failed", "unreviewed-proposal")
    );
    assert!(
        doc.metrics["structure"]
            .detail
            .contains(&"tables:2->3".to_owned())
    );
    Ok(())
}

#[test]
fn disagreeing_with_reviewed_ground_truth_fails_the_measurement() -> TestResult {
    let bench = Bench::new()?;
    let mut engine = FakeEngine::default();
    let mut record = bench.propose(&mut engine, "doc.docx", "docx-tabular", &[])?;
    bench.review(&mut record)?;
    bench.store(&record)?;

    let agreeing = bench.measure(&mut engine)?;
    assert!(agreeing.passed);
    assert_eq!(agreeing.reviewed_ground_truth_records, 1);
    let doc = document(&agreeing, Bench::DOC)?;
    assert_eq!(
        metric(doc, "text")?,
        pair("passed", "reviewed-ground-truth")
    );
    assert_eq!(metric(doc, "diff")?, pair("not_measured", "none"));
    assert_eq!(
        class_summary(&agreeing, "docx-tabular")?.evidence,
        "reviewed-ground-truth"
    );

    let mut regressed = FakeEngine {
        warnings: vec!["DOCX_FONT_SUBSTITUTED", "DOCX_LAYOUT_PAGINATED"],
        ..FakeEngine::default()
    };
    let report = bench.measure(&mut regressed)?;
    assert!(!report.passed);
    let doc = document(&report, Bench::DOC)?;
    assert_eq!(
        metric(doc, "diagnostics")?,
        pair("failed", "reviewed-ground-truth")
    );
    assert_eq!(
        doc.metrics["diagnostics"].detail,
        vec!["unexpected:DOCX_LAYOUT_PAGINATED"]
    );
    assert_eq!(class_summary(&report, "docx-tabular")?.failed_documents, 1);
    validate_report(&report)?;
    Ok(())
}

#[test]
fn a_review_must_be_documented_before_a_record_counts_as_reviewed() -> TestResult {
    let bench = Bench::new()?;
    let mut engine = FakeEngine::default();
    let proposal = bench.propose(&mut engine, "doc.docx", "docx-tabular", &[])?;
    let taxonomy = taxonomy();

    let mut claimed = proposal.clone();
    claimed.review.status = "reviewed".into();
    claimed.review.reviewer = Some("reviewer-01".into());
    assert_eq!(
        code(validate_record(
            claimed.clone(),
            &taxonomy,
            Some(&bench.register)
        )),
        Some("UNREVIEWED_GROUND_TRUTH")
    );

    let mut reviewed = proposal.clone();
    bench.review(&mut reviewed)?;
    assert!(validate_record(reviewed.clone(), &taxonomy, Some(&bench.register)).is_ok());

    let mut tampered = reviewed.clone();
    if let Some(evidence) = tampered.review.evidence.as_mut() {
        evidence.sha256 = digest(b"another note");
    }
    assert_eq!(
        code(validate_record(tampered, &taxonomy, Some(&bench.register))),
        Some("UNREVIEWED_GROUND_TRUTH")
    );

    let short = "short note";
    std::fs::write(bench.register.join("reviews/short.md"), short)?;
    let mut brief = reviewed.clone();
    brief.review.evidence = Some(xtask::corpus::Input {
        file: "reviews/short.md".into(),
        sha256: digest(short.as_bytes()),
    });
    assert_eq!(
        code(validate_record(brief, &taxonomy, Some(&bench.register))),
        Some("UNREVIEWED_GROUND_TRUTH")
    );

    let mut anonymous = reviewed.clone();
    anonymous.review.reviewer = None;
    assert_eq!(
        code(validate_record(anonymous, &taxonomy, Some(&bench.register))),
        Some("UNREVIEWED_GROUND_TRUTH")
    );

    let mut half = proposal.clone();
    half.review.reviewer = Some("reviewer-01".into());
    assert_eq!(
        code(validate_record(half, &taxonomy, None)),
        Some("INVALID_GROUND_TRUTH")
    );

    let mut rejected = proposal.clone();
    rejected.class = "unsupported-format".into();
    rejected.format = "invalid".into();
    assert_eq!(
        code(validate_record(rejected, &taxonomy, None)),
        Some("INVALID_GROUND_TRUTH")
    );

    save(&bench.register.join("misnamed.json"), &proposal)?;
    assert_eq!(
        code(load_register(&bench.register, &taxonomy)),
        Some("INVALID_GROUND_TRUTH")
    );
    std::fs::remove_file(bench.register.join("misnamed.json"))?;

    let mut conflicting = proposal;
    conflicting.class = "docx-structured".into();
    bench.store(&conflicting)?;
    assert_eq!(
        code(bench.try_measure(&mut engine)),
        Some("QUALITY_CLASS_CONFLICT")
    );
    Ok(())
}

#[test]
fn engine_invariant_violations_fail_the_measurement() -> TestResult {
    let bench = Bench::new()?;
    let variants: Vec<(FakeEngine, &[u8], &str)> = vec![
        (
            FakeEngine {
                alternate_text: true,
                ..FakeEngine::default()
            },
            Bench::DOC,
            "determinism",
        ),
        (
            FakeEngine {
                tables: 0,
                ..FakeEngine::default()
            },
            Bench::DOC,
            "classification",
        ),
        (
            FakeEngine {
                render_px: (40, 20),
                ..FakeEngine::default()
            },
            Bench::DOC,
            "render_geometry",
        ),
        (
            FakeEngine {
                self_diff_changes: 1,
                ..FakeEngine::default()
            },
            Bench::DOC,
            "diff_identity",
        ),
        (
            FakeEngine {
                rejection: (11, "MALFORMED_DOCUMENT"),
                ..FakeEngine::default()
            },
            Bench::BAD,
            "rejection",
        ),
    ];
    for (mut engine, data, name) in variants {
        let report = bench.measure(&mut engine)?;
        assert!(!report.passed, "{name}");
        let (status, basis) = metric(document(&report, data)?, name)?;
        assert_eq!(status, "failed", "{name}");
        assert!(
            basis == "engine-invariant" || basis == "corpus-expectation",
            "{name}"
        );
        validate_report(&report)?;
    }
    let tables = FakeEngine {
        tables: 0,
        ..FakeEngine::default()
    };
    let report = bench.measure(&mut tables.clone())?;
    assert_eq!(
        document(&report, Bench::DOC)?.metrics["classification"].detail,
        vec!["tables:below-minimum"]
    );
    Ok(())
}

#[test]
fn an_engine_failure_is_recorded_for_its_document_only() -> TestResult {
    let bench = Bench::new()?;
    let mut engine = FakeEngine {
        failing_extension: Some(".pdf"),
        ..FakeEngine::default()
    };
    let report = bench.measure(&mut engine)?;
    assert!(!report.passed);
    let table = document(&report, Bench::TABLE)?;
    assert_eq!(table.error_code.as_deref(), Some("QUALITY_ENGINE_ERROR"));
    assert!(
        table
            .metrics
            .values()
            .all(|outcome| outcome.status == "not_measured")
    );
    assert!(document(&report, Bench::DOC)?.error_code.is_none());
    assert_eq!(class_summary(&report, "pdf-tabular")?.failed_documents, 1);
    Ok(())
}

#[test]
fn comparisons_report_regressions_improvements_and_coverage() -> TestResult {
    let bench = Bench::new()?;
    let baseline = bench.measure(&mut FakeEngine::default())?;
    let same = compare(&baseline, &baseline)?;
    assert!(same.passed);
    assert!(same.regressions.is_empty() && same.improvements.is_empty());

    let candidate = bench.measure(&mut FakeEngine {
        alternate_text: true,
        failing_extension: Some(".pdf"),
        ..FakeEngine::default()
    })?;
    let comparison = compare(&baseline, &candidate)?;
    assert!(!comparison.passed);
    let regressed: Vec<(&str, &str)> = comparison
        .regressions
        .iter()
        .map(|change| (change.class.as_str(), change.metric.as_str()))
        .collect();
    assert!(regressed.contains(&("docx-tabular", "determinism")));
    assert!(regressed.contains(&("pdf-tabular", "measurement")));
    assert!(regressed.contains(&("pdf-tabular", "classification")));

    let recovered = compare(&candidate, &baseline)?;
    assert!(recovered.passed);
    assert!(
        recovered
            .improvements
            .iter()
            .any(|change| change.metric == "determinism")
    );

    let mut shrunk = baseline.clone();
    shrunk.documents.retain(|document| document.format != "pdf");
    let coverage = compare(&baseline, &shrunk)?;
    assert!(!coverage.passed);
    assert_eq!(coverage.removed_documents.len(), 1);
    assert!(compare(&shrunk, &baseline)?.passed);

    let mut forged = candidate;
    forged.passed = true;
    assert_eq!(
        code(compare(&baseline, &forged)),
        Some("INVALID_QUALITY_REPORT")
    );
    Ok(())
}
