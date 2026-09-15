use docsight_core::{DocsightError, Document, DocumentFormat, DocumentSource};
use docsight_layout::{LaidOutDocument, layout_docx};
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;

pub fn ingest(source: &DocumentSource) -> Result<Document, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => Ok(ingest_docx(source)?.document),
        DocumentFormat::Pdf => PdfDocument::open(source)?.to_document(),
    }
}

pub fn ingest_docx(source: &DocumentSource) -> Result<LaidOutDocument, DocsightError> {
    layout_docx(parse_docx(source)?)
}
