use docsight_core::{DocumentObject, DocumentSource, compute_evidence};
use docsight_ingest::ingest;
use docsight_search::{FindMode, FindObjectKind, FindRequest, context_neighborhood, find};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

const FIXTURES: &[&str] = &[
    "sample_features.docx",
    "sample_headings.docx",
    "sample_tables.docx",
    "sample_semantic.pdf",
    "sample_table_ruled.pdf",
    "sample_table_alignment.pdf",
    "sample_table_two_ruled.pdf",
    "synthetic_table.pdf",
];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn literal(pattern: &str) -> FindRequest {
    FindRequest {
        pattern: pattern.to_owned(),
        mode: FindMode::Literal,
        ignore_case: false,
        kinds: BTreeSet::new(),
        pages: None,
        region: None,
    }
}

#[test]
fn every_emitted_object_identifier_resolves_to_its_own_object()
-> Result<(), Box<dyn std::error::Error>> {
    for name in FIXTURES {
        let document = ingest(&DocumentSource::open(fixture(name))?)?;
        let ids = document.object_ids();
        assert!(!ids.is_empty(), "{name} exposes no object identifiers");
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "{name} emits duplicate identifiers"
        );
        for id in ids {
            let object = document
                .resolve_object(id.as_str())
                .ok_or_else(|| format!("{name}: emitted identifier {id} does not resolve"))?;
            assert_eq!(object.id(), id, "{name}: {id} resolved to another object");
        }
    }
    Ok(())
}

#[test]
fn every_object_identifier_is_accepted_by_evidence_and_context()
-> Result<(), Box<dyn std::error::Error>> {
    for name in FIXTURES {
        let source = DocumentSource::open(fixture(name))?;
        let document = ingest(&source)?;
        for id in document.object_ids() {
            let evidence = compute_evidence(&document, &source, id, None, 1.0)
                .map_err(|error| format!("{name}: evidence rejected {id}: {error}"))?;
            assert_eq!(&evidence.object_id, id);
            let viewport = context_neighborhood(&document, id.as_str(), false)
                .map_err(|error| format!("{name}: context rejected {id}: {error}"))?;
            assert!(
                viewport.objects.iter().any(|entry| &entry.object.id == id),
                "{name}: context for {id} does not contain the target"
            );
        }
    }
    Ok(())
}

#[test]
fn table_cells_report_their_containing_table() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_tables.docx"))?;
    let document = ingest(&source)?;
    let mut checked = 0usize;
    for id in document.object_ids() {
        let Some(DocumentObject::TableCell { table, cell }) = document.resolve_object(id.as_str())
        else {
            continue;
        };
        let evidence = compute_evidence(&document, &source, id, None, 1.0)?;
        assert_eq!(evidence.container_id.as_ref(), Some(&table.id));
        assert_eq!(evidence.page, table.page);
        assert_eq!(
            evidence.text_fragment,
            cell.text.chars().take(300).collect::<String>()
        );
        checked += 1;
    }
    assert!(checked > 0, "the fixture no longer exercises table cells");
    Ok(())
}

#[test]
fn find_returns_cells_instead_of_whole_tables() -> Result<(), Box<dyn std::error::Error>> {
    let document = ingest(&DocumentSource::open(fixture("sample_semantic.pdf"))?)?;
    let result = find(&document, &literal("Latency"))?;

    assert_eq!(result.total_matches, 1);
    let found = &result.matches[0];
    assert_eq!(found.kind, FindObjectKind::TableCell);
    assert!(found.container_id.is_some());
    assert_eq!(found.matched.text, "Latency");
    let bbox = found.bbox.ok_or("cell match has no geometry")?;
    let table = document
        .resolve_object(found.container_id.as_ref().ok_or("no container")?.as_str())
        .and_then(|table| table.bbox())
        .ok_or("table geometry missing")?;
    assert!(
        bbox.height() < table.height(),
        "a cell match must be smaller than its table"
    );
    Ok(())
}

#[test]
fn find_reports_character_offsets_for_multibyte_text() -> Result<(), Box<dyn std::error::Error>> {
    let document = ingest(&DocumentSource::open(fixture("sample_semantic.pdf"))?)?;
    let result = find(&document, &literal("native"))?;
    let found = result.matches.first().ok_or("no match")?;
    let object = document
        .resolve_object(found.object_id.as_str())
        .ok_or("match does not resolve")?;
    let text = object.text();
    let extracted: String = text
        .chars()
        .skip(found.matched.start_char)
        .take(found.matched.end_char - found.matched.start_char)
        .collect();
    assert_eq!(extracted, found.matched.text);
    assert!(text.contains(&format!(
        "{}{}{}",
        found.context_before, found.matched.text, found.context_after
    )));
    Ok(())
}

#[test]
fn find_matches_case_insensitively_and_by_regex() -> Result<(), Box<dyn std::error::Error>> {
    let document = ingest(&DocumentSource::open(fixture("sample_semantic.pdf"))?)?;

    assert_eq!(find(&document, &literal("latency"))?.total_matches, 0);
    let mut folded = literal("LATENCY");
    folded.ignore_case = true;
    assert_eq!(find(&document, &folded)?.total_matches, 1);

    let mut expression = literal(r"\d+ms");
    expression.mode = FindMode::Regex;
    let result = find(&document, &expression)?;
    assert_eq!(result.total_matches, 1);
    assert_eq!(result.matches[0].matched.text, "12ms");
    Ok(())
}

#[test]
fn find_filters_by_kind_page_and_region() -> Result<(), Box<dyn std::error::Error>> {
    let document = ingest(&DocumentSource::open(fixture("sample_semantic.pdf"))?)?;

    let mut headings = literal("System");
    headings.kinds = [FindObjectKind::Heading].into_iter().collect();
    assert!(
        find(&document, &headings)?
            .matches
            .iter()
            .all(|found| found.kind == FindObjectKind::Heading)
    );

    let mut missing_page = literal("System");
    missing_page.pages = Some((2, 2));
    assert_eq!(find(&document, &missing_page)?.total_matches, 0);

    let mut region_without_page = literal("System");
    region_without_page.region = Some(docsight_core::Rect::new(0.0, 0.0, 100.0, 100.0)?);
    assert!(find(&document, &region_without_page).is_err());

    let mut top_region = literal("Report");
    top_region.pages = Some((1, 1));
    top_region.region = Some(docsight_core::Rect::new(0.0, 0.0, 595.0, 80.0)?);
    assert_eq!(find(&document, &top_region)?.total_matches, 1);
    let mut bottom_region = top_region.clone();
    bottom_region.region = Some(docsight_core::Rect::new(0.0, 700.0, 595.0, 842.0)?);
    assert_eq!(find(&document, &bottom_region)?.total_matches, 0);
    Ok(())
}

#[test]
fn find_rejects_invalid_requests_with_typed_errors() -> Result<(), Box<dyn std::error::Error>> {
    let document = ingest(&DocumentSource::open(fixture("sample_semantic.pdf"))?)?;

    assert!(matches!(
        find(&document, &literal("")),
        Err(docsight_core::DocsightError::InvalidArgument { .. })
    ));
    let mut broken = literal("(unclosed");
    broken.mode = FindMode::Regex;
    assert!(matches!(
        find(&document, &broken),
        Err(docsight_core::DocsightError::InvalidArgument { .. })
    ));
    let oversized = literal(&"x".repeat(docsight_search::MAX_FIND_PATTERN_BYTES + 1));
    assert!(matches!(
        find(&document, &oversized),
        Err(docsight_core::DocsightError::ResourceLimit { .. })
    ));
    Ok(())
}

#[test]
fn find_to_crop_to_evidence_works_end_to_end_from_the_cli() -> Result<(), Box<dyn std::error::Error>>
{
    let path = fixture("sample_semantic.pdf");
    let path = path.to_str().ok_or("invalid path")?;
    let directory = tempfile::tempdir()?;

    let found = docsight()
        .args(["--agent", "find", path, "12ms"])
        .output()?;
    assert!(found.status.success());
    let found: serde_json::Value = serde_json::from_slice(&found.stdout)?;
    let object_id = found["result"]["matches"][0]["object_id"]
        .as_str()
        .ok_or("find returned no object id")?
        .to_owned();

    let crop_path = directory.path().join("match.png");
    let crop = docsight()
        .args([
            "--agent",
            "crop",
            path,
            "--object",
            &object_id,
            "--out",
            crop_path.to_str().ok_or("invalid crop path")?,
        ])
        .output()?;
    assert!(
        crop.status.success(),
        "crop rejected an identifier returned by find: {}",
        String::from_utf8_lossy(&crop.stderr)
    );
    assert!(std::fs::metadata(&crop_path)?.len() > 0);

    let evidence = docsight()
        .args(["--agent", "evidence", path, &object_id])
        .output()?;
    assert!(evidence.status.success());
    let evidence: serde_json::Value = serde_json::from_slice(&evidence.stdout)?;
    assert_eq!(evidence["result"]["object_id"], object_id.as_str());
    assert_eq!(evidence["result"]["kind"], "table_cell");

    let context = docsight()
        .args(["--agent", "context", path, &object_id])
        .output()?;
    assert!(context.status.success());
    let context: serde_json::Value = serde_json::from_slice(&context.stdout)?;
    assert_eq!(context["result"]["context"]["target"]["kind"], "table_cell");
    assert_eq!(
        context["result"]["context"]["containers"]["object"]["kind"],
        "table"
    );
    Ok(())
}

#[test]
fn find_output_is_deterministic_and_continuable() -> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_tables.docx");
    let path = path.to_str().ok_or("invalid path")?;

    let first = docsight()
        .args(["--agent", "find", path, "e", "--max-items", "2"])
        .output()?;
    let second = docsight()
        .args(["--agent", "find", path, "e", "--max-items", "2"])
        .output()?;
    assert!(first.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "find output is not deterministic"
    );

    let page: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(page["limits"]["truncated"], true);
    let token = page["limits"]["continuation_token"]
        .as_str()
        .ok_or("truncated find has no continuation token")?;
    let next = docsight()
        .args([
            "--agent",
            "find",
            path,
            "e",
            "--max-items",
            "2",
            "--continue",
            token,
        ])
        .output()?;
    assert!(
        next.status.success(),
        "continuation was rejected: {}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next: serde_json::Value = serde_json::from_slice(&next.stdout)?;
    assert_ne!(
        page["result"]["matches"][0]["object_id"], next["result"]["matches"][0]["object_id"],
        "continuation returned the same first match"
    );
    Ok(())
}

#[test]
fn an_object_without_its_own_geometry_names_the_block_to_crop()
-> Result<(), Box<dyn std::error::Error>> {
    let path = fixture("sample_features.docx");
    let document = ingest(&DocumentSource::open(&path)?)?;
    let link = document.links.first().ok_or("fixture has no hyperlink")?;
    let directory = tempfile::tempdir()?;

    let crop = docsight()
        .args([
            "--agent",
            "crop",
            path.to_str().ok_or("invalid path")?,
            "--object",
            link.id.as_str(),
            "--out",
            directory
                .path()
                .join("link.png")
                .to_str()
                .ok_or("invalid out path")?,
        ])
        .output()?;
    assert_eq!(crop.status.code(), Some(20));
    let error: serde_json::Value = serde_json::from_slice(&crop.stderr)?;
    let message = error["error"]["message"]
        .as_str()
        .ok_or("error has no message")?;
    assert!(
        message.contains("crop its anchoring block p_"),
        "the error must name the block to crop instead: {message}"
    );
    Ok(())
}
