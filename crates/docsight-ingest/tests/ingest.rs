use docsight_core::{DocumentFormat, DocumentSource, IR_SCHEMA_VERSION, canonical_violations};
use docsight_ingest::{ingest, ingest_docx};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

#[test]
fn ingests_docx_through_layout() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_headings.docx"))?;
    let document = ingest(&source)?;

    assert_eq!(document.format, DocumentFormat::Docx);
    assert_eq!(document.version.schema_version, IR_SCHEMA_VERSION);
    assert!(
        !document.pages.is_empty(),
        "DOCX ingestion must return a paginated document"
    );
    assert!(
        document
            .blocks
            .iter()
            .all(|block| block.page.is_some() && block.bbox.is_some()),
        "every DOCX block must carry page placement and geometry after ingestion"
    );
    assert!(canonical_violations(&document).is_empty());
    Ok(())
}

#[test]
fn ingests_pdf_into_the_same_representation() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::open(fixture("sample_semantic.pdf"))?;
    let document = ingest(&source)?;

    assert_eq!(document.format, DocumentFormat::Pdf);
    assert_eq!(document.version.schema_version, IR_SCHEMA_VERSION);
    assert!(!document.pages.is_empty());
    assert!(!document.blocks.is_empty());
    assert!(canonical_violations(&document).is_empty());
    Ok(())
}

#[test]
fn docx_layout_material_agrees_with_the_ingested_document() -> Result<(), Box<dyn std::error::Error>>
{
    let source = DocumentSource::open(fixture("sample_tables.docx"))?;
    let laid_out = ingest_docx(&source)?;
    let document = ingest(&source)?;

    assert_eq!(laid_out.document, document);
    assert_eq!(laid_out.pages.len(), document.pages.len());
    Ok(())
}

#[test]
fn repeated_ingestion_is_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    for name in ["sample_features.docx", "sample_semantic.pdf"] {
        let source = DocumentSource::open(fixture(name))?;
        let first = serde_json::to_vec(&ingest(&source)?)?;
        let second = serde_json::to_vec(&ingest(&source)?)?;
        assert_eq!(first, second, "{name} ingestion is not deterministic");
    }
    Ok(())
}

#[test]
fn rejects_bytes_that_identify_no_supported_format() {
    assert!(DocumentSource::from_bytes(b"not a document".to_vec()).is_err());
}

#[test]
fn publishes_every_declared_ingestion_limit() {
    use docsight_ingest::INGESTION_LIMITS;

    assert!(!INGESTION_LIMITS.is_empty());
    for limit in INGESTION_LIMITS {
        assert!(limit.value > 0, "{} declares a zero limit", limit.name);
        assert!(
            !limit.effect.is_empty(),
            "{} declares no effect",
            limit.name
        );
        assert!(
            limit
                .applies_to
                .split(',')
                .all(|format| matches!(format, "docx" | "pdf")),
            "{} declares an unknown format",
            limit.name
        );
    }

    let mut names: Vec<&str> = INGESTION_LIMITS.iter().map(|limit| limit.name).collect();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "ingestion limit names must be unique");
}

#[test]
fn declared_limits_match_the_enforced_constants() {
    use docsight_ingest::INGESTION_LIMITS;

    let declared = |name: &str| {
        INGESTION_LIMITS
            .iter()
            .find(|limit| limit.name == name)
            .map(|limit| limit.value)
    };
    assert_eq!(
        declared("document_bytes"),
        Some(docsight_core::MAX_INSPECT_BYTES)
    );
    assert_eq!(
        declared("package_entries"),
        Some(docsight_ooxml::MAX_PACKAGE_ENTRIES as u64)
    );
    assert_eq!(
        declared("package_compression_ratio"),
        Some(docsight_ooxml::MAX_COMPRESSION_RATIO)
    );
    assert_eq!(
        declared("xml_element_depth"),
        Some(docsight_ooxml::MAX_XML_DEPTH as u64)
    );
    assert_eq!(
        declared("layout_pages"),
        Some(docsight_layout::MAX_LAYOUT_PAGES as u64)
    );
    assert_eq!(declared("pdf_pages"), Some(docsight_pdf::MAX_PAGES as u64));
    assert_eq!(
        declared("pdf_page_annotations"),
        Some(docsight_pdf::MAX_ANNOTATIONS as u64)
    );
    assert_eq!(
        declared("embedded_image_pixels"),
        Some(docsight_core::MAX_IMAGE_PIXELS)
    );
    assert_eq!(
        declared("jpeg_progressive_scans"),
        Some(docsight_core::MAX_JPEG_SCANS as u64)
    );
}
