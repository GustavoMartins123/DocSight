use docsight_core::{
    BlockContent, BlockKind, CoverageMetric, CoverageStatus, DocumentSource, OverlayKind,
    compute_coverage,
};
use docsight_ingest::ingest;
use std::path::PathBuf;

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

fn assert_metric_is_sound(metric: &CoverageMetric, label: &str) {
    assert!(
        (0.0..=1.0).contains(&metric.score),
        "{label} reports a score outside the unit interval: {}",
        metric.score
    );
    match metric.status {
        CoverageStatus::Exact => assert!(
            metric.score >= 0.999,
            "{label} claims exact coverage with score {}",
            metric.score
        ),
        CoverageStatus::Approximated | CoverageStatus::Unsupported => assert!(
            !metric.reason_codes.is_empty() || metric.score < 0.999,
            "{label} reports degraded coverage without a score or a reason code"
        ),
        CoverageStatus::Inferred => {}
    }
}

#[test]
fn every_fixture_reports_a_computable_coverage_profile() -> Result<(), Box<dyn std::error::Error>> {
    for name in FIXTURES {
        let source = DocumentSource::open(fixture(name))?;
        let document = ingest(&source)?;
        let coverage = compute_coverage(&document, &source, None, true, 1.0)?;

        assert_eq!(coverage.document_sha256, source.sha256());
        assert_eq!(coverage.pages.len(), document.pages.len());
        for (label, metric) in [
            ("text", &coverage.global.text),
            ("structure", &coverage.global.structure),
            ("geometry", &coverage.global.geometry),
            ("visual", &coverage.global.visual),
            ("resource", &coverage.global.resource),
        ] {
            assert_metric_is_sound(metric, &format!("{name} global {label}"));
        }
        for page in &coverage.pages {
            for (label, metric) in [
                ("text", &page.text),
                ("structure", &page.structure),
                ("geometry", &page.geometry),
                ("visual", &page.visual),
                ("resource", &page.resource),
            ] {
                assert_metric_is_sound(metric, &format!("{name} page {} {label}", page.page));
            }
        }
        assert!(
            (0.0..=1.0).contains(&coverage.global.overall_fidelity),
            "{name} reports an out-of-range overall fidelity"
        );
    }
    Ok(())
}

#[test]
fn unsupported_feature_count_matches_the_reported_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    for name in FIXTURES {
        let source = DocumentSource::open(fixture(name))?;
        let document = ingest(&source)?;
        let coverage = compute_coverage(&document, &source, None, false, 1.0)?;

        assert!(
            coverage.unsupported_feature_count <= document.warnings.len(),
            "{name} counts more unsupported features than it emitted diagnostics"
        );
        if coverage.unsupported_feature_count == 0 {
            assert!(
                coverage.reason_codes.is_empty(),
                "{name} reports reason codes without counting an unsupported feature"
            );
        }
    }
    Ok(())
}

#[test]
fn resource_coverage_is_exact_for_a_rasterizable_figure() -> Result<(), Box<dyn std::error::Error>>
{
    let source = DocumentSource::open(fixture("sample_features.docx"))?;
    let document = ingest(&source)?;
    let coverage = compute_coverage(&document, &source, None, false, 1.0)?;

    assert!(
        document.figures().count() > 0,
        "the fixture no longer exercises figure resources"
    );
    assert!(
        !document
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER"),
        "a PNG figure must not be reported as a placeholder"
    );
    assert_eq!(coverage.global.resource.status, CoverageStatus::Exact);
    assert_eq!(coverage.global.resource.score, 1.0);
    Ok(())
}

#[test]
fn resource_coverage_is_exact_without_resource_references() -> Result<(), Box<dyn std::error::Error>>
{
    let source = DocumentSource::open(fixture("sample_semantic.pdf"))?;
    let document = ingest(&source)?;
    let coverage = compute_coverage(&document, &source, None, false, 1.0)?;

    assert_eq!(document.figures().count(), 0);
    assert_eq!(coverage.global.resource.status, CoverageStatus::Exact);
    assert_eq!(coverage.global.resource.score, 1.0);
    Ok(())
}

#[test]
fn docx_structure_covers_the_declared_logical_elements() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_features.docx"))?;
    let document = ingest(&source)?;

    assert!(document.paragraphs().count() > 0, "paragraphs are missing");
    assert!(document.headings().count() > 0, "headings are missing");
    assert!(document.figures().count() > 0, "figures are missing");
    assert!(document.notes().count() > 0, "footnotes are missing");
    assert!(!document.links.is_empty(), "hyperlinks are missing");
    assert!(!document.comments.is_empty(), "comments are missing");
    assert!(
        document.tracked_changes.insertions > 0 || document.tracked_changes.deletions > 0,
        "tracked changes are missing"
    );
    assert!(
        document.pages.iter().any(|page| page
            .overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::Header)),
        "headers are missing"
    );
    assert!(
        document.pages.iter().any(|page| page
            .overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::Footer)),
        "footers are missing"
    );
    assert!(
        !document.resources.is_empty(),
        "image resources are missing"
    );
    Ok(())
}

#[test]
fn docx_tables_preserve_grid_spans_and_list_numbering() -> Result<(), Box<dyn std::error::Error>> {
    let tables_source = DocumentSource::open(fixture("sample_tables.docx"))?;
    let tables = ingest(&tables_source)?;
    assert!(tables.tables().count() > 0, "no structural table");
    for (_, table) in tables.tables() {
        assert!(table.rows > 0 && table.columns > 0);
        for cell in &table.cells {
            assert!(
                cell.row_span >= 1 && cell.column_span >= 1,
                "every cell must declare a span of at least one"
            );
            assert!(
                cell.row < table.rows && cell.column < table.columns,
                "cells must stay inside the declared grid"
            );
            assert!(
                cell.row + cell.row_span <= table.rows
                    && cell.column + cell.column_span <= table.columns,
                "a merged cell must not span past the declared grid"
            );
        }
    }
    assert!(
        tables
            .tables()
            .flat_map(|(_, table)| table.cells.iter())
            .any(|cell| cell.row_span > 1 || cell.column_span > 1),
        "the corpus no longer exercises merged cells"
    );

    assert!(
        tables.list_items().count() > 0,
        "list items are missing from the corpus"
    );
    assert!(
        tables
            .list_items()
            .any(|(_, item)| item.marker.is_some() || item.ordered.is_some()),
        "list numbering was not resolved for any item"
    );
    Ok(())
}

#[test]
fn pdf_structure_carries_reading_order_and_page_local_geometry()
-> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_semantic.pdf"))?;
    let document = ingest(&source)?;
    let page = document.pages.first().ok_or("no page")?;

    assert!(!document.blocks.is_empty(), "no reconstructed blocks");
    assert!(
        document.headings().count() > 0,
        "no heading was reconstructed"
    );
    assert!(document.tables().count() > 0, "no table was inferred");

    let mut previous = 0;
    for block in &document.blocks {
        assert!(
            block.reading_order > previous,
            "PDF blocks must carry a strictly increasing reading order"
        );
        previous = block.reading_order;

        let bbox = block.bbox.ok_or("PDF block without geometry")?;
        assert!(
            bbox.x0 >= 0.0 && bbox.y0 >= 0.0,
            "PDF geometry must be page-local"
        );
        assert!(
            bbox.x1 <= page.width_pt + 1.0 && bbox.y1 <= page.height_pt + 1.0,
            "PDF geometry must stay inside the page box"
        );
        assert!(
            block.confidence > 0.0 && block.confidence <= 1.0,
            "every reconstructed PDF block must carry confidence"
        );
    }
    Ok(())
}

#[test]
fn inferred_pdf_tables_declare_their_detector() -> Result<(), Box<dyn std::error::Error>> {
    for name in [
        "sample_table_ruled.pdf",
        "sample_table_alignment.pdf",
        "sample_table_two_ruled.pdf",
    ] {
        let source = DocumentSource::open(fixture(name))?;
        let document = ingest(&source)?;
        for (block, table) in document.tables() {
            assert!(
                table.detector.is_some(),
                "{name} infers a table without naming its detector"
            );
            assert!(
                block.confidence < 1.0,
                "{name} reports an inferred table as fully certain"
            );
        }
    }
    Ok(())
}

#[test]
fn every_block_kind_declared_by_the_ir_is_reachable_or_documented()
-> Result<(), Box<dyn std::error::Error>> {
    let mut seen: Vec<BlockKind> = Vec::new();
    for name in FIXTURES {
        let source = DocumentSource::open(fixture(name))?;
        for block in ingest(&source)?.blocks {
            let kind = match block.content {
                BlockContent::Paragraph(_) => BlockKind::Paragraph,
                BlockContent::Heading(_) => BlockKind::Heading,
                BlockContent::ListItem(_) => BlockKind::ListItem,
                BlockContent::Table(_) => BlockKind::Table,
                BlockContent::Figure(_) => BlockKind::Figure,
                BlockContent::Shape(_) => BlockKind::Shape,
                BlockContent::Note(_) => BlockKind::Note,
                BlockContent::Unknown(_) => BlockKind::Unknown,
            };
            if !seen.contains(&kind) {
                seen.push(kind);
            }
        }
    }
    for kind in [
        BlockKind::Paragraph,
        BlockKind::Heading,
        BlockKind::ListItem,
        BlockKind::Table,
        BlockKind::Figure,
        BlockKind::Note,
    ] {
        assert!(
            seen.contains(&kind),
            "the validation corpus no longer exercises {kind:?}"
        );
    }
    assert!(
        !seen.contains(&BlockKind::Shape),
        "Shape blocks are now produced; PRODUCT_SCOPE.md must stop declaring them absent"
    );
    Ok(())
}
