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
