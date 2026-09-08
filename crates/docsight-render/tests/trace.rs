use docsight_core::{DocumentSource, Rect};
use docsight_render::{
    RenderRequest, RenderTarget,
    trace::{
        ProofBundle, TraceArtifact, TraceDecisionStatus, TraceDisplayOperation, TraceResourceKind,
        TraceTableSizing, create_proof_bundle, record_trace, verify_proof_bundle, verify_trace,
    },
};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use zip::write::SimpleFileOptions;

#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

#[test]
fn docx_trace_replays_from_its_embedded_source_deterministically()
-> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_headings.docx"))?;
    let request = RenderRequest {
        target: RenderTarget::Page { page: 1 },
        dpi: 72,
    };
    let trace = record_trace(&source, &request)?;
    let bytes = trace.to_bytes()?;
    let repeated = record_trace(&source, &request)?.to_bytes()?;

    assert_eq!(bytes, repeated);
    assert!(!trace.manifest.glyph_runs.is_empty());
    assert_eq!(
        trace.manifest.decision_coverage.display_list_operations,
        TraceDecisionStatus::Verified
    );
    assert_eq!(
        trace.manifest.decision_coverage.pagination,
        TraceDecisionStatus::Verified
    );

    let read = TraceArtifact::from_bytes(&bytes)?;
    let verification = verify_trace(&read)?;
    assert!(verification.valid);
    assert_eq!(verification.document_sha256, source.sha256());
    assert_eq!(verification.raster_sha256, trace.manifest.raster.sha256);
    Ok(())
}

#[test]
fn proof_bundle_verifies_embedded_evidence_and_optional_crop()
-> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_headings.docx"))?;
    let request = RenderRequest {
        target: RenderTarget::Object {
            id: "h_515ad605791c12fc496c1c18d79f6526".to_owned(),
        },
        dpi: 72,
    };
    let bundle = create_proof_bundle(&source, &request, true)?;
    let bytes = bundle.to_bytes()?;
    let read = ProofBundle::from_bytes(&bytes)?;
    let verification = verify_proof_bundle(&read)?;

    assert!(verification.valid);
    assert!(verification.crop_verified);
    assert_eq!(verification.evidence_count, 1);
    assert_eq!(verification.bundle_sha256, docsight_sha256(&bytes));
    Ok(())
}

#[test]
fn pdf_trace_replays_complete_applicable_decisions() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(pdf_fixture::sample_pdf())?;
    let request = RenderRequest {
        target: RenderTarget::Page { page: 1 },
        dpi: 72,
    };
    let trace = record_trace(&source, &request)?;

    assert!(!trace.manifest.glyph_runs.is_empty());
    assert_eq!(trace.manifest.schema, "docsight.trace/v2");
    assert_eq!(
        trace.manifest.decision_coverage.display_list_operations,
        TraceDecisionStatus::Verified
    );
    assert_eq!(
        trace.manifest.decision_coverage.pagination,
        TraceDecisionStatus::NotApplicable
    );
    assert_eq!(
        trace.manifest.decision_coverage.line_breaks,
        TraceDecisionStatus::NotApplicable
    );
    assert_eq!(
        trace.manifest.decision_coverage.table_sizing,
        TraceDecisionStatus::NotApplicable
    );
    assert!(trace.manifest.table_sizing.is_empty());
    assert!(trace.manifest.pagination.is_empty());
    assert!(
        trace
            .manifest
            .warnings
            .iter()
            .all(|warning| warning.code != "TRACE_DECISION_PARTIAL")
    );
    assert!(
        trace
            .manifest
            .display_list
            .operations
            .iter()
            .any(|operation| matches!(operation, TraceDisplayOperation::Fill { .. }))
    );
    assert!(
        trace
            .manifest
            .display_list
            .operations
            .iter()
            .any(|operation| matches!(operation, TraceDisplayOperation::Text { .. }))
    );
    assert!(
        trace
            .manifest
            .resources
            .iter()
            .any(|resource| { resource.kind == TraceResourceKind::Font && resource.name == "F1" })
    );
    assert!(
        trace
            .manifest
            .display_list
            .operations
            .iter()
            .all(|operation| !matches!(operation, TraceDisplayOperation::Border { .. }))
    );
    assert!(verify_trace(&trace)?.valid);
    Ok(())
}

#[test]
fn trace_rejects_decision_records_marked_not_applicable() -> Result<(), Box<dyn std::error::Error>>
{
    let source = DocumentSource::from_bytes(pdf_fixture::sample_pdf())?;
    let request = RenderRequest {
        target: RenderTarget::Page { page: 1 },
        dpi: 72,
    };
    let mut trace = record_trace(&source, &request)?;
    trace.manifest.table_sizing.push(TraceTableSizing {
        object_id: "tbl_invalid".to_owned(),
        page: 1,
        bbox: None,
        rows: 1,
        columns: 1,
        column_widths_pt: None,
        detector: None,
    });

    assert!(trace.to_bytes().is_err());
    Ok(())
}

#[test]
fn region_bundle_records_each_intersecting_semantic_object()
-> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_headings.docx"))?;
    let request = RenderRequest {
        target: RenderTarget::Region {
            page: 1,
            bbox: Rect::new(72.0, 72.0, 540.0, 150.0)?,
        },
        dpi: 72,
    };
    let bundle = create_proof_bundle(&source, &request, false)?;

    assert!(!bundle.manifest.evidence.is_empty());
    assert!(bundle.manifest.crop.is_none());
    assert!(verify_proof_bundle(&bundle)?.valid);
    Ok(())
}

#[test]
fn malformed_proof_manifest_is_rejected_before_verification()
-> Result<(), Box<dyn std::error::Error>> {
    let source_bytes = std::fs::read(fixture("sample_headings.docx"))?;
    let cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("manifest.json", options)?;
    writer.write_all(br#"{}"#)?;
    writer.start_file("source.bin", options)?;
    writer.write_all(&source_bytes)?;
    let bytes = writer.finish()?.into_inner();

    assert!(ProofBundle::from_bytes(&bytes).is_err());
    Ok(())
}

#[test]
fn fuzz_proof_bundle_bytes_fail_closed_without_panicking() -> Result<(), Box<dyn std::error::Error>>
{
    let source = DocumentSource::open(fixture("sample_headings.docx"))?;
    let request = RenderRequest {
        target: RenderTarget::Object {
            id: "h_515ad605791c12fc496c1c18d79f6526".to_owned(),
        },
        dpi: 72,
    };
    let bytes = create_proof_bundle(&source, &request, true)?.to_bytes()?;

    for offset in (0..bytes.len()).step_by(131) {
        let mut corrupted = bytes.clone();
        corrupted[offset] ^= 0x5a;
        assert!(std::panic::catch_unwind(|| ProofBundle::from_bytes(&corrupted)).is_ok());
    }
    Ok(())
}

fn docsight_sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
