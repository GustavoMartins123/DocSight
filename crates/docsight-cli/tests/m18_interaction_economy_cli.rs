use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("fixtures").join("validation").join(name)
}

#[derive(Debug, Deserialize)]
struct Corpus {
    schema: String,
    policy: Policy,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
struct Policy {
    maximum_cli_invocations: usize,
    scratch_parser_invocations: usize,
    allowed_executable: String,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    id: String,
    task: String,
    document_formats: Vec<String>,
    max_cli_invocations: usize,
    max_serialized_bytes: usize,
    max_render_requests: usize,
    required_evidence: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ScenarioRun {
    cli_invocations: usize,
    serialized_bytes: usize,
    render_requests: usize,
    executables: BTreeSet<String>,
    evidence: BTreeSet<String>,
}

impl ScenarioRun {
    fn invoke(&mut self, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
        let output = docsight().args(args).output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        self.executables.insert("docsight".to_owned());
        self.cli_invocations = self
            .cli_invocations
            .checked_add(1)
            .ok_or("CLI invocation count overflow")?;
        self.serialized_bytes = self
            .serialized_bytes
            .checked_add(output.stdout.len())
            .ok_or("serialized byte count overflow")?;
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn invoke_rendering(&mut self, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
        self.render_requests = self
            .render_requests
            .checked_add(1)
            .ok_or("render request count overflow")?;
        self.invoke(args)
    }

    fn add_evidence(&mut self, evidence: &[&str]) {
        self.evidence
            .extend(evidence.iter().map(|value| (*value).to_owned()));
    }
}

fn read_corpus() -> Result<Corpus, Box<dyn std::error::Error>> {
    let path = workspace().join("fixtures/conformance/m18-interaction-economy.json");
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn locate_table() -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("DOCX fixture path")?;
    let mut run = ScenarioRun::default();
    let value = run.invoke(&[
        "--agent", "resolve", path, "--text", "Col 1", "--kind", "table", "--budget", "4kb",
    ])?;
    assert_eq!(value["result"]["status"], "resolved");
    assert_eq!(
        value["result"]["candidates"][0]["object"]["id"],
        "tbl_6c22c8dd17adf7e32604b29241d9ab99"
    );
    assert!(value["result"]["candidates"][0]["matched_range"].is_object());
    assert!(
        value["result"]["candidates"][0]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
    );
    run.add_evidence(&[
        "deterministic_object_id",
        "matched_range",
        "ranking_reason_codes",
    ]);
    Ok(run)
}

fn locate_pdf_table() -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let path = fixture("sample_table_ruled.pdf");
    let path = path.to_str().ok_or("PDF fixture path")?;
    let mut run = ScenarioRun::default();
    let value = run.invoke(&[
        "--agent",
        "resolve",
        path,
        "--text",
        "Category Target Actual",
        "--kind",
        "table",
        "--budget",
        "4kb",
    ])?;
    let candidate = &value["result"]["candidates"][0];
    assert_eq!(value["result"]["status"], "resolved");
    assert_eq!(
        candidate["object"]["id"],
        "tbl_a28732332b543d0c98460b91973e0708"
    );
    assert!(
        candidate["object"]["confidence"]
            .as_f64()
            .is_some_and(|score| score > 0.9)
    );
    assert!(candidate["matched_range"].is_object());
    run.add_evidence(&[
        "deterministic_object_id",
        "inference_confidence",
        "matched_range",
    ]);
    Ok(run)
}

fn read_merged_cells() -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("DOCX fixture path")?;
    let mut run = ScenarioRun::default();
    let value = run.invoke(&[
        "--agent",
        "context",
        path,
        "tbl_6c22c8dd17adf7e32604b29241d9ab99",
        "--include",
        "content",
        "--budget",
        "24kb",
    ])?;
    let context = &value["result"]["context"];
    assert_eq!(context["content"]["type"], "table");
    assert!(
        context["content"]["cells"]
            .as_array()
            .is_some_and(|cells| cells.iter().any(|cell| {
                cell["column_span"].as_u64().is_some_and(|span| span > 1)
                    || cell["row_span"].as_u64().is_some_and(|span| span > 1)
            }))
    );
    assert_eq!(
        context["target"]["source"],
        "/word/document.xml::body/tbl[1]"
    );
    run.add_evidence(&[
        "canonical_table_content",
        "merged_cell_span",
        "source_provenance",
    ]);
    Ok(run)
}

fn inspect_page_context() -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let path = path.to_str().ok_or("DOCX fixture path")?;
    let mut run = ScenarioRun::default();
    let value = run.invoke(&["--agent", "peek", path, "--page", "1", "--budget", "8kb"])?;
    let objects = value["result"]["objects"]
        .as_array()
        .ok_or("peek objects")?;
    assert!(!objects.is_empty());
    assert_eq!(value["limits"]["truncated"], false);
    assert_eq!(
        value["result"]["total_objects"].as_u64(),
        u64::try_from(objects.len()).ok()
    );
    assert!(
        objects
            .iter()
            .all(|entry| entry["object"]["bbox"].is_object())
    );
    assert!(objects.windows(2).all(|pair| {
        pair[0]["object"]["reading_order"]
            .as_u64()
            .zip(pair[1]["object"]["reading_order"].as_u64())
            .is_some_and(|(left, right)| left <= right)
    }));
    let kinds = objects
        .iter()
        .filter_map(|entry| entry["object"]["kind"].as_str())
        .collect::<BTreeSet<_>>();
    assert!(kinds.contains("heading"));
    assert!(kinds.contains("paragraph"));
    run.add_evidence(&["canonical_geometry", "reading_order", "semantic_kinds"]);
    Ok(run)
}

fn verify_visual_region(directory: &Path) -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let path = fixture("sample_headings.docx");
    let path = path.to_str().ok_or("DOCX fixture path")?;
    let bundle = directory.join("heading.dse");
    let bundle = bundle.to_str().ok_or("bundle path")?;
    let mut run = ScenarioRun::default();
    let created = run.invoke_rendering(&[
        "--agent",
        "bundle",
        path,
        "--object",
        "h_515ad605791c12fc496c1c18d79f6526",
        "--dpi",
        "72",
        "--include-crop",
        "--out",
        bundle,
        "--budget",
        "8kb",
    ])?;
    assert_eq!(created["result"]["crop_included"], true);
    assert_eq!(
        created["result"]["output_sha256"].as_str().map(str::len),
        Some(64)
    );
    assert_eq!(
        created["result"]["trace_raster_sha256"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    let verified = run.invoke(&["--agent", "verify", bundle, "--budget", "8kb"])?;
    assert_eq!(verified["result"]["verification"]["valid"], true);
    assert_eq!(verified["result"]["verification"]["crop_verified"], true);
    run.add_evidence(&["crop_digest", "proof_bundle_digest", "replay_verification"]);
    Ok(run)
}

fn compare_generated_documents() -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let before = before.to_str().ok_or("before fixture path")?;
    let after = after.to_str().ok_or("after fixture path")?;
    let mut run = ScenarioRun::default();
    let value = run.invoke(&[
        "--agent",
        "diff",
        before,
        after,
        "--select",
        "summary,before_document,after_document",
        "--budget",
        "8kb",
    ])?;
    assert_ne!(
        value["result"]["before_document"]["sha256"],
        value["result"]["after_document"]["sha256"]
    );
    assert!(
        value["result"]["summary"]["semantic_changes"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        value["result"]["summary"]["evidence_limited_changes"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    run.add_evidence(&[
        "document_digests",
        "semantic_change_count",
        "evidence_limited_count",
    ]);
    Ok(run)
}

fn execute_scenario(
    scenario: &Scenario,
    directory: &Path,
) -> Result<ScenarioRun, Box<dyn std::error::Error>> {
    match scenario.id.as_str() {
        "locate-table" => locate_table(),
        "locate-pdf-table" => locate_pdf_table(),
        "read-merged-cells" => read_merged_cells(),
        "inspect-page-context" => inspect_page_context(),
        "verify-visual-region" => verify_visual_region(directory),
        "compare-generated-documents" => compare_generated_documents(),
        id => Err(format!("unknown interaction-economy scenario: {id}").into()),
    }
}

#[test]
fn interaction_economy_corpus_is_complete_and_fail_closed() -> Result<(), Box<dyn std::error::Error>>
{
    let corpus = read_corpus()?;
    assert_eq!(corpus.schema, "docsight.interaction-economy/v1");
    assert_eq!(corpus.policy.maximum_cli_invocations, 2);
    assert_eq!(corpus.policy.scratch_parser_invocations, 0);
    assert_eq!(corpus.policy.allowed_executable, "docsight");
    let expected = [
        "compare-generated-documents",
        "inspect-page-context",
        "locate-pdf-table",
        "locate-table",
        "read-merged-cells",
        "verify-visual-region",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let actual = corpus
        .scenarios
        .iter()
        .map(|scenario| scenario.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
    assert!(corpus.scenarios.iter().all(|scenario| {
        !scenario.task.is_empty()
            && !scenario.document_formats.is_empty()
            && scenario.max_cli_invocations <= corpus.policy.maximum_cli_invocations
            && scenario.max_serialized_bytes > 0
    }));
    let formats = corpus
        .scenarios
        .iter()
        .flat_map(|scenario| scenario.document_formats.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    assert_eq!(formats, BTreeSet::from(["docx", "pdf"]));
    Ok(())
}

#[test]
fn normal_agent_workflows_meet_the_interaction_economy_gates()
-> Result<(), Box<dyn std::error::Error>> {
    let corpus = read_corpus()?;
    let temporary = tempfile::tempdir()?;
    for scenario in &corpus.scenarios {
        let run = execute_scenario(scenario, temporary.path())?;
        assert!(
            run.cli_invocations <= scenario.max_cli_invocations,
            "{} used {} CLI invocations; maximum is {}",
            scenario.id,
            run.cli_invocations,
            scenario.max_cli_invocations
        );
        assert!(
            run.serialized_bytes <= scenario.max_serialized_bytes,
            "{} emitted {} bytes; maximum is {}",
            scenario.id,
            run.serialized_bytes,
            scenario.max_serialized_bytes
        );
        assert!(
            run.render_requests <= scenario.max_render_requests,
            "{} used {} render requests; maximum is {}",
            scenario.id,
            run.render_requests,
            scenario.max_render_requests
        );
        assert_eq!(
            run.executables,
            BTreeSet::from([corpus.policy.allowed_executable.clone()]),
            "{}",
            scenario.id
        );
        assert!(scenario.required_evidence.is_subset(&run.evidence));
    }
    Ok(())
}
