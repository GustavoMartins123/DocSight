use docsight_core::{DocsightError, Document, DocumentFormat, DocumentSource};
use docsight_layout::{LaidOutDocument, layout_docx};
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;

pub fn ingest(source: &DocumentSource) -> Result<Document, DocsightError> {
    ingest_with_password(source, b"")
}

pub fn ingest_with_password(
    source: &DocumentSource,
    password: &[u8],
) -> Result<Document, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => Ok(ingest_docx(source)?.document),
        DocumentFormat::Pdf => PdfDocument::open_with_password(source, password)?.to_document(),
    }
}

pub fn ingest_docx(source: &DocumentSource) -> Result<LaidOutDocument, DocsightError> {
    layout_docx(parse_docx(source)?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct IngestionLimit {
    pub name: &'static str,
    pub applies_to: &'static str,
    pub value: u64,
    pub unit: &'static str,
    pub effect: &'static str,
}

pub const INGESTION_LIMITS: &[IngestionLimit] = &[
    IngestionLimit {
        name: "document_bytes",
        applies_to: "docx,pdf",
        value: docsight_core::MAX_INSPECT_BYTES,
        unit: "bytes",
        effect: "the document is rejected before parsing",
    },
    IngestionLimit {
        name: "package_entries",
        applies_to: "docx",
        value: docsight_ooxml::MAX_PACKAGE_ENTRIES as u64,
        unit: "entries",
        effect: "the package is rejected before any part is read",
    },
    IngestionLimit {
        name: "package_uncompressed_bytes",
        applies_to: "docx",
        value: docsight_ooxml::MAX_TOTAL_UNCOMPRESSED_BYTES,
        unit: "bytes",
        effect: "the package is rejected before any part is read",
    },
    IngestionLimit {
        name: "package_compression_ratio",
        applies_to: "docx",
        value: docsight_ooxml::MAX_COMPRESSION_RATIO,
        unit: "ratio",
        effect: "the package is rejected before any part is read",
    },
    IngestionLimit {
        name: "xml_part_bytes",
        applies_to: "docx",
        value: docsight_ooxml::MAX_XML_PART_BYTES,
        unit: "bytes",
        effect: "the part is rejected and the document is not parsed",
    },
    IngestionLimit {
        name: "binary_part_bytes",
        applies_to: "docx",
        value: docsight_ooxml::MAX_BINARY_PART_BYTES,
        unit: "bytes",
        effect: "the part is rejected and the document is not parsed",
    },
    IngestionLimit {
        name: "xml_element_depth",
        applies_to: "docx",
        value: docsight_ooxml::MAX_XML_DEPTH as u64,
        unit: "elements",
        effect: "the part is rejected and the document is not parsed",
    },
    IngestionLimit {
        name: "xml_nodes",
        applies_to: "docx",
        value: docsight_ooxml::MAX_XML_NODES as u64,
        unit: "nodes",
        effect: "the part is rejected and the document is not parsed",
    },
    IngestionLimit {
        name: "xml_token_bytes",
        applies_to: "docx",
        value: docsight_ooxml::MAX_XML_TOKEN_BYTES as u64,
        unit: "bytes",
        effect: "the part is rejected and the document is not parsed",
    },
    IngestionLimit {
        name: "layout_pages",
        applies_to: "docx",
        value: docsight_layout::MAX_LAYOUT_PAGES as u64,
        unit: "pages",
        effect: "layout stops and the document is rejected",
    },
    IngestionLimit {
        name: "pdf_pages",
        applies_to: "pdf",
        value: docsight_pdf::MAX_PAGES as u64,
        unit: "pages",
        effect: "the document is rejected while the page tree is collected",
    },
    IngestionLimit {
        name: "pdf_page_annotations",
        applies_to: "pdf",
        value: docsight_pdf::MAX_ANNOTATIONS as u64,
        unit: "annotations",
        effect: "the document is rejected while the page tree is collected",
    },
    IngestionLimit {
        name: "pdf_content_operations",
        applies_to: "pdf",
        value: docsight_pdf::PDF_MAX_CONTENT_OPERATIONS as u64,
        unit: "operations",
        effect: "the page content stream is rejected",
    },
    IngestionLimit {
        name: "pdf_font_bytes",
        applies_to: "pdf",
        value: docsight_pdf::PDF_MAX_FONT_BYTES as u64,
        unit: "bytes",
        effect: "the embedded font program is rejected",
    },
    IngestionLimit {
        name: "raster_pixels",
        applies_to: "docx,pdf",
        value: docsight_pdf::PDF_MAX_RASTER_PIXELS,
        unit: "pixels",
        effect: "the raster request is rejected before allocation",
    },
    IngestionLimit {
        name: "embedded_image_pixels",
        applies_to: "docx",
        value: docsight_core::MAX_IMAGE_PIXELS,
        unit: "pixels",
        effect: "the embedded image is not decoded and the figure is rendered as a placeholder",
    },
    IngestionLimit {
        name: "jpeg_progressive_scans",
        applies_to: "docx",
        value: docsight_core::MAX_JPEG_SCANS as u64,
        unit: "scans",
        effect: "the embedded JPEG is not decoded and the figure is rendered as a placeholder",
    },
];
