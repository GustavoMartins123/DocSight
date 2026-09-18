#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::beta;
use xtask::corpus::{self, Case, CaseOutcome, Expectation};
use xtask::readiness::{CRITERIA, REVIEWS, Report, assess, load_policy};
use xtask::release::TARGETS;
use xtask::tooling::common::{digest, read_json, sha256_file, workspace_root, workspace_version};
use xtask::tooling::process::ProcessLimits;
use xtask::validation;

fn criterion<'a>(report: &'a Report, name: &str) -> TestResult<(bool, Option<&'a str>)> {
    let found = report
        .criteria
        .iter()
        .find(|criterion| criterion.name == name)
        .ok_or("criterion")?;
    Ok((found.passed, found.error_code.as_deref()))
}

fn policy(minimum_beta_participants: u64, reviews: &[&str]) -> Value {
    json!({
        "schema": "docsight.readiness-policy/v1",
        "minimum_beta_participants": minimum_beta_participants,
        "minimum_corpus_documents": 2,
        "minimum_real_documents_per_format": 1,
        "required_reviews": reviews,
    })
}

struct Candidate {
    fixture: Fixture,
    packages: BTreeMap<String, String>,
}

impl Candidate {
    fn new() -> TestResult<Self> {
        let fixture = Fixture::new()?;
        let root = fixture.root.clone();
        save(
            &root.join("release/readiness-policy.json"),
            &policy(5, &REVIEWS),
        )?;
        save(
            &root.join("release/known-gaps.json"),
            &json!({"schema": "docsight.known-gaps/v1", "items": [
                {"id": "docx-multi-section-geometry", "blocking": true, "status": "resolved", "source": "BACKLOG.md"},
                {"id": "ingestion-limit-overrides", "blocking": false, "status": "deferred-until-real-case", "source": "BACKLOG.md"}
            ]}),
        )?;
        save(
            &root.join("release/beta-issues.json"),
            &json!({"schema": "docsight.beta-issues/v1", "issues": []}),
        )?;
        fs::create_dir_all(root.join("release"))?;
        fs::copy(
            workspace_root().join("release/document-classes.json"),
            root.join("release/document-classes.json"),
        )?;
        let mut candidate = Self {
            fixture,
            packages: BTreeMap::new(),
        };
        candidate.write_validation()?;
        candidate.write_packages()?;
        candidate.write_beta(5)?;
        candidate.write_corpus("consented-real")?;
        candidate.write_reviews(true)?;
        Ok(candidate)
    }

    fn root(&self) -> &Path {
        &self.fixture.root
    }

    fn evidence(&self) -> PathBuf {
        self.fixture.root.join("evidence")
    }

    fn assess(&self) -> TestResult<Report> {
        Ok(assess(&self.evidence(), REVISION, self.root())?)
    }

    fn write_validation(&self) -> TestResult {
        let mut runner = Callback(
            |arguments: &[OsString],
             _: &Path,
             _: &ProcessLimits,
             _: Option<&BTreeMap<OsString, OsString>>| {
                if arguments.iter().any(|argument| argument == "rev-parse") {
                    return Ok(outcome(0, format!("{REVISION}\n").into_bytes(), Vec::new()));
                }
                Ok(outcome(0, Vec::new(), Vec::new()))
            },
        );
        let report = validation::run_with(
            &self.evidence().join("validation"),
            self.root(),
            &self.root().join("maint"),
            &mut runner,
        )?;
        assert!(report.passed);
        Ok(())
    }

    fn write_packages(&mut self) -> TestResult {
        for target in TARGETS {
            let archive = self.fixture.package(target, "evidence")?;
            let receipt = self.fixture.receipt(&archive)?;
            self.packages
                .insert(target.to_owned(), receipt.archive_sha256);
        }
        Ok(())
    }

    fn beta_report(&self, index: usize, target: &str) -> TestResult<beta::Report> {
        Ok(beta::Report {
            schema: "docsight.beta-report/v1".into(),
            version: workspace_version().into(),
            revision: REVISION.into(),
            target: target.into(),
            archive_sha256: self.packages.get(target).ok_or("package")?.clone(),
            participant: format!("beta-{index:03}"),
            operation: "inspect".into(),
            experience: "clear".into(),
            outcome: "success".into(),
            exit_code: 0,
            elapsed_ms: 10,
            stdout_bytes: 100,
            stderr_bytes: 0,
            document_size_bytes: 1000,
            document_sha256: None,
            diagnostic_codes: Vec::new(),
            unknown_diagnostic_count: 0,
        })
    }

    fn write_beta(&self, participants: usize) -> TestResult {
        let directory = self.evidence().join("beta");
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        for index in 1..=participants {
            let target = TARGETS[(index - 1) % TARGETS.len()];
            save(
                &directory.join(format!("observation-{index}.json")),
                &self.beta_report(index, target)?,
            )?;
        }
        Ok(())
    }

    fn cases(origin: &str) -> Vec<Case> {
        [("real-docx", "docx"), ("real-pdf", "pdf")]
            .into_iter()
            .map(|(id, format)| Case {
                id: id.into(),
                file: format!("corpus/{id}.{format}"),
                sha256: digest(id.as_bytes()),
                origin: origin.into(),
                format: format.into(),
                class: format!("{format}-structured"),
                operation: "inspect".into(),
                expected: Expectation {
                    exit_code: 0,
                    diagnostic_codes: Vec::new(),
                    pointer_equals: BTreeMap::new(),
                    repeat: 2,
                },
                reference: None,
            })
            .collect()
    }

    fn outcome(case: &Case) -> CaseOutcome {
        CaseOutcome {
            id: case.id.clone(),
            origin: case.origin.clone(),
            format: case.format.clone(),
            class: case.class.clone(),
            document_sha256: case.sha256.clone(),
            reference_sha256: None,
            operation: case.operation.clone(),
            passed: true,
            error_code: None,
            attempts: case.expected.repeat,
            elapsed_ms: 4,
            output_sha256: Some("d".repeat(64)),
            diagnostic_codes: case.expected.diagnostic_codes.clone(),
        }
    }

    fn write_corpus(&self, origin: &str) -> TestResult {
        let manifest_path = self.evidence().join("corpus-manifest.json");
        let cases = Self::cases(origin);
        save(
            &manifest_path,
            &corpus::Manifest {
                schema: "docsight.corpus/v2".into(),
                cases: cases.clone(),
            },
        )?;
        let manifest_sha256 = sha256_file(&manifest_path)?;
        for target in TARGETS {
            let report = corpus::Report {
                schema: "docsight.corpus-report/v1".into(),
                version: workspace_version().into(),
                revision: REVISION.into(),
                target: target.into(),
                archive_sha256: self.packages.get(target).ok_or("package")?.clone(),
                manifest_sha256: manifest_sha256.clone(),
                cases: cases.iter().map(Self::outcome).collect(),
                passed: true,
            };
            save(
                &self.evidence().join(format!("corpus-{target}.json")),
                &report,
            )?;
        }
        Ok(())
    }

    fn write_reviews(&self, approved: bool) -> TestResult {
        let mut reviews = serde_json::Map::new();
        for name in REVIEWS {
            let file = format!("reviews/{name}.md");
            let path = self.evidence().join(&file);
            let text = format!(
                "# {name}\n\nProcedure: synthetic unit-test review record.\nObservations: none.\nLimitations: not a real human review.\n"
            );
            fs::create_dir_all(path.parent().ok_or("review parent")?)?;
            fs::write(&path, text)?;
            reviews.insert(
                name.to_owned(),
                json!({
                    "approved": approved,
                    "reviewer": "maintainer-a",
                    "evidence": {"file": file, "sha256": sha256_file(&path)?},
                }),
            );
        }
        save(
            &self.evidence().join("reviews.json"),
            &json!({
                "schema": "docsight.release-reviews/v1",
                "version": workspace_version(),
                "revision": REVISION,
                "policy_sha256": sha256_file(&self.root().join("release/readiness-policy.json"))?,
                "reviews": reviews,
            }),
        )
    }
}

#[test]
fn complete_consistent_evidence_satisfies_every_criterion() -> TestResult {
    let candidate = Candidate::new()?;
    let report = candidate.assess()?;
    let names: Vec<_> = report
        .criteria
        .iter()
        .map(|criterion| criterion.name.as_str())
        .collect();
    assert_eq!(names, CRITERIA);
    assert!(
        report.ready_for_v1,
        "{:?}",
        report
            .criteria
            .iter()
            .filter(|criterion| !criterion.passed)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        report.policy_sha256,
        sha256_file(&candidate.root().join("release/readiness-policy.json"))?
    );
    Ok(())
}

#[test]
fn unapproved_reviews_and_changed_policies_block_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    candidate.write_reviews(false)?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "manual-reviews")?,
        (false, Some("PENDING_MANUAL_REVIEW"))
    );
    assert!(!report.ready_for_v1);

    let candidate = Candidate::new()?;
    let mut changed = policy(5, &REVIEWS);
    changed["minimum_corpus_documents"] = json!(3);
    save(
        &candidate.root().join("release/readiness-policy.json"),
        &changed,
    )?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "manual-reviews")?,
        (false, Some("UNREVIEWED_READINESS_POLICY"))
    );
    assert!(!report.ready_for_v1);
    Ok(())
}

#[test]
fn insufficient_or_unbound_beta_observations_block_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    candidate.write_beta(4)?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "beta-observations")?,
        (false, Some("INSUFFICIENT_BETA_PARTICIPANTS"))
    );

    let candidate = Candidate::new()?;
    let mut foreign = candidate.beta_report(6, TARGETS[0])?;
    foreign.archive_sha256 = "e".repeat(64);
    save(
        &candidate.evidence().join("beta/observation-6.json"),
        &foreign,
    )?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "beta-observations")?,
        (false, Some("BETA_PACKAGE_MISMATCH"))
    );
    assert!(!report.ready_for_v1);
    Ok(())
}

#[test]
fn synthetic_or_incomplete_corpus_evidence_blocks_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    candidate.write_corpus("synthetic")?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "broad-corpus")?,
        (false, Some("INSUFFICIENT_REAL_CORPUS"))
    );

    let candidate = Candidate::new()?;
    let path = candidate
        .evidence()
        .join(format!("corpus-{}.json", TARGETS[3]));
    let mut stored: corpus::Report = serde_json::from_value(read_json(&path)?)?;
    stored.cases[1].passed = false;
    stored.cases[1].error_code = Some("CORPUS_ASSERTION".into());
    save(&path, &stored)?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "broad-corpus")?,
        (false, Some("FAILED_CORPUS_CASE"))
    );
    assert!(!report.ready_for_v1);
    Ok(())
}

#[test]
fn tampered_or_foreign_workspace_validation_blocks_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    let log = candidate
        .evidence()
        .join("validation/cargo_test.stdout.log");
    fs::write(&log, b"edited after validation")?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "workspace-validation")?,
        (false, Some("EVIDENCE_DIGEST_MISMATCH"))
    );

    let candidate = Candidate::new()?;
    let path = candidate.evidence().join("validation/validation.json");
    let mut stored = read_json(&path)?;
    stored["revision"] = json!("c".repeat(40));
    save(&path, &stored)?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "workspace-validation")?,
        (false, Some("EVIDENCE_CANDIDATE_MISMATCH"))
    );
    assert!(!report.ready_for_v1);
    Ok(())
}

#[test]
fn missing_native_packages_block_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    let archive = candidate.evidence().join(format!(
        "docsight-{}-{}.zip",
        workspace_version(),
        TARGETS[4]
    ));
    fs::remove_file(archive)?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "five-native-packages")?,
        (false, Some("MISSING_NATIVE_PACKAGES"))
    );
    assert!(!report.ready_for_v1);
    Ok(())
}

#[test]
fn open_gaps_and_unresolved_beta_issues_block_readiness() -> TestResult {
    let candidate = Candidate::new()?;
    save(
        &candidate.root().join("release/known-gaps.json"),
        &json!({"schema": "docsight.known-gaps/v1", "items": [
            {"id": "content-addressed-cache", "blocking": true, "status": "open", "source": "BACKLOG.md"}
        ]}),
    )?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "known-v1-gaps")?,
        (false, Some("OPEN_V1_PRODUCT_GAPS"))
    );

    let variants = [
        (
            json!({"id": "beta-1", "severity": "high", "status": "open", "regression_case": null, "before_evidence": null}),
            "OPEN_BETA_BLOCKER",
        ),
        (
            json!({"id": "beta-2", "severity": "blocker", "status": "resolved", "regression_case": null, "before_evidence": null}),
            "MISSING_BETA_REGRESSION",
        ),
        (
            json!({"id": "beta-3", "severity": "medium", "status": "resolved", "regression_case": "real-docx", "before_evidence": null}),
            "MISSING_PRE_FIX_FAILURE",
        ),
    ];
    for (issue, expected) in variants {
        let candidate = Candidate::new()?;
        save(
            &candidate.root().join("release/beta-issues.json"),
            &json!({"schema": "docsight.beta-issues/v1", "issues": [issue]}),
        )?;
        let report = candidate.assess()?;
        assert_eq!(
            criterion(&report, "beta-regressions")?,
            (false, Some(expected))
        );
        assert!(!report.ready_for_v1);
    }

    let candidate = Candidate::new()?;
    save(
        &candidate.root().join("release/beta-issues.json"),
        &json!({"schema": "docsight.beta-issues/v1", "issues": [
            {"id": "beta-4", "severity": "low", "status": "open", "regression_case": null, "before_evidence": null}
        ]}),
    )?;
    assert!(candidate.assess()?.ready_for_v1);
    Ok(())
}

#[test]
fn resolved_issues_require_a_distinct_reproduced_pre_fix_failure() -> TestResult {
    let candidate = Candidate::new()?;
    let case = Candidate::cases("consented-real")
        .into_iter()
        .next()
        .ok_or("case")?;
    let mut failure = Candidate::outcome(&case);
    failure.passed = false;
    failure.error_code = Some("CORPUS_ASSERTION".into());
    failure.attempts = 1;
    failure.output_sha256 = None;
    let before = corpus::Report {
        schema: "docsight.corpus-report/v1".into(),
        version: workspace_version().into(),
        revision: "f".repeat(40),
        target: TARGETS[1].into(),
        archive_sha256: "a".repeat(64),
        manifest_sha256: "b".repeat(64),
        cases: vec![failure],
        passed: false,
    };
    let path = candidate.evidence().join("regressions/beta-5-before.json");
    save(&path, &before)?;
    let issue = |evidence_sha256: String| {
        json!({"schema": "docsight.beta-issues/v1", "issues": [{
            "id": "beta-5",
            "severity": "blocker",
            "status": "resolved",
            "regression_case": case.id,
            "before_evidence": {"file": "regressions/beta-5-before.json", "sha256": evidence_sha256},
        }]})
    };
    save(
        &candidate.root().join("release/beta-issues.json"),
        &issue(sha256_file(&path)?),
    )?;
    assert!(candidate.assess()?.ready_for_v1);

    let mut same_revision = before.clone();
    same_revision.revision = REVISION.into();
    fs::remove_file(&path)?;
    save(&path, &same_revision)?;
    save(
        &candidate.root().join("release/beta-issues.json"),
        &issue(sha256_file(&path)?),
    )?;
    let report = candidate.assess()?;
    assert_eq!(
        criterion(&report, "beta-regressions")?,
        (false, Some("MISSING_PRE_FIX_FAILURE"))
    );
    Ok(())
}

#[test]
fn repository_policy_is_valid_and_resolved_gaps_leave_evidence_blocking_v1() -> TestResult {
    let root = workspace_root();
    let loaded = load_policy(&root)?;
    assert_eq!(loaded.required_reviews, REVIEWS);
    let directory = tempfile::tempdir()?;
    let report = assess(&directory.path().join("absent"), REVISION, &root)?;
    assert!(!report.ready_for_v1);
    assert_eq!(report.criteria.len(), CRITERIA.len());
    assert_eq!(criterion(&report, "known-v1-gaps")?, (true, None));
    assert!(
        !criterion(&report, "workspace-validation")?.0,
        "absent candidate evidence must keep V1 blocked"
    );
    assert_eq!(criterion(&report, "beta-regressions")?, (true, None));
    Ok(())
}

#[test]
fn readiness_policy_cannot_lower_participants_or_drop_reviews() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.root.join("release/readiness-policy.json");
    save(&path, &policy(4, &REVIEWS))?;
    assert_eq!(
        code(load_policy(&fixture.root)),
        Some("INVALID_READINESS_POLICY")
    );
    fs::remove_file(&path)?;
    save(&path, &policy(5, &REVIEWS[..5]))?;
    assert_eq!(
        code(load_policy(&fixture.root)),
        Some("INVALID_READINESS_POLICY")
    );
    Ok(())
}
