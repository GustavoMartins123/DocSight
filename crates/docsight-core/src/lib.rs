pub mod ir;

pub use ir::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const MAX_INSPECT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Docx,
    Pdf,
}

impl Display for DocumentFormat {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Docx => formatter.write_str("DOCX"),
            Self::Pdf => formatter.write_str("PDF"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub effect: String,
}

#[derive(Debug, Error)]
pub enum DocsightError {
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },
    #[error("I/O operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("document format is not supported or cannot be identified from its bytes")]
    UnsupportedFormat,
    #[error("resource limit exceeded for {resource}: maximum {limit}")]
    ResourceLimit { resource: String, limit: u64 },
    #[error("malformed document: {message}")]
    MalformedDocument { message: String },
    #[error("encrypted documents require a password and are not supported")]
    EncryptedDocument,
    #[error("{backend} backend failed: {message}")]
    BackendFailure { backend: String, message: String },
    #[error("{operation} is not supported for {format} documents")]
    UnsupportedOperation {
        operation: String,
        format: DocumentFormat,
    },
    #[error("feature is not supported: {feature}")]
    UnsupportedFeature { feature: String },
    #[error("object was not found: {object}")]
    ObjectNotFound { object: String },
}

impl DocsightError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidArgument { .. } => 2,
            Self::UnsupportedFormat | Self::UnsupportedOperation { .. } => 10,
            Self::MalformedDocument { .. } => 11,
            Self::EncryptedDocument => 12,
            Self::ResourceLimit { .. } => 13,
            Self::UnsupportedFeature { .. } => 20,
            Self::ObjectNotFound { .. } => 21,
            Self::BackendFailure { .. } => 30,
            Self::Io { .. } => 40,
        }
    }

    pub fn diagnostic(&self) -> Diagnostic {
        let (code, effect) = match self {
            Self::InvalidArgument { .. } => ("USAGE", "the requested operation was not performed"),
            Self::UnsupportedFormat => ("UNSUPPORTED_FORMAT", "the document was not inspected"),
            Self::UnsupportedOperation { .. } => (
                "UNSUPPORTED_FORMAT",
                "the requested operation was not performed",
            ),
            Self::UnsupportedFeature { .. } => (
                "UNSUPPORTED_FEATURE",
                "the requested operation was not performed",
            ),
            Self::MalformedDocument { .. } => (
                "MALFORMED_DOCUMENT",
                "the document was rejected during parsing",
            ),
            Self::EncryptedDocument => (
                "ENCRYPTED_DOCUMENT",
                "the document was rejected before inspection",
            ),
            Self::ResourceLimit { .. } => {
                ("RESOURCE_LIMIT", "the document was rejected before parsing")
            }
            Self::ObjectNotFound { .. } => ("OBJECT_NOT_FOUND", "no document object was returned"),
            Self::BackendFailure { .. } => (
                "BACKEND_FAILURE",
                "the requested operation could not be completed",
            ),
            Self::Io { .. } => ("IO_ERROR", "the requested file could not be read"),
        };
        Diagnostic {
            code: code.to_owned(),
            severity: DiagnosticSeverity::Error,
            message: self.to_string(),
            effect: effect.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSource {
    bytes: Vec<u8>,
    format: DocumentFormat,
    digest: String,
}

impl DocumentSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DocsightError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_INSPECT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| DocsightError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if bytes.len() as u64 > MAX_INSPECT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "document bytes".to_owned(),
                limit: MAX_INSPECT_BYTES,
            });
        }
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, DocsightError> {
        let format = sniff_format(&bytes)?;
        let digest = sha256_hex(&bytes);
        Ok(Self {
            bytes,
            format,
            digest,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn sha256(&self) -> &str {
        &self.digest
    }

    pub fn id(&self) -> String {
        format!("doc_{}", &self.digest[..12])
    }

    pub fn size_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn object_id(&self, prefix: &str, source_path: &str) -> ObjectId {
        ObjectId::new(prefix, &self.digest, source_path)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn new(prefix: &str, document_digest: &str, source_path: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(document_digest.as_bytes());
        hasher.update([0]);
        hasher.update(source_path.as_bytes());
        let digest = hasher.finalize();
        let suffix: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self(format!("{prefix}_{suffix}"))
    }

    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ObjectId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<Self, DocsightError> {
        if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
            return Err(DocsightError::InvalidArgument {
                message: "rectangle coordinates must be finite".to_owned(),
            });
        }
        if x0 >= x1 || y0 >= y1 {
            return Err(DocsightError::InvalidArgument {
                message: "rectangle must have positive width and height".to_owned(),
            });
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1);
        let y1 = self.y1.min(other.y1);
        if x0 < x1 && y0 < y1 {
            Some(Self { x0, y0, x1, y1 })
        } else {
            None
        }
    }
}

pub fn write_all(path: &Path, bytes: &[u8]) -> Result<(), DocsightError> {
    let mut file = File::create(path).map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes).map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sniff_format(bytes: &[u8]) -> Result<DocumentFormat, DocsightError> {
    if bytes.starts_with(b"%PDF-") {
        return Ok(DocumentFormat::Pdf);
    }
    let is_zip = bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08");
    if is_zip
        && contains_bytes(bytes, b"[Content_Types].xml")
        && contains_bytes(bytes, b"word/document.xml")
    {
        return Ok(DocumentFormat::Docx);
    }
    Err(DocsightError::UnsupportedFormat)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{DocsightError, DocumentFormat, DocumentSource, Rect};

    fn docx_signature() -> Vec<u8> {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(b"[Content_Types].xml");
        bytes.extend_from_slice(b"word/document.xml");
        bytes
    }

    #[test]
    fn identifies_pdf_by_magic_bytes() -> Result<(), DocsightError> {
        let source = DocumentSource::from_bytes(b"%PDF-1.7\n".to_vec())?;
        assert_eq!(source.format(), DocumentFormat::Pdf);
        Ok(())
    }

    #[test]
    fn identifies_docx_by_package_markers() -> Result<(), DocsightError> {
        let source = DocumentSource::from_bytes(docx_signature())?;
        assert_eq!(source.format(), DocumentFormat::Docx);
        Ok(())
    }

    #[test]
    fn rejects_extension_like_content_without_magic_bytes() {
        let error = DocumentSource::from_bytes(b"report.docx".to_vec());
        assert!(matches!(error, Err(DocsightError::UnsupportedFormat)));
    }

    #[test]
    fn creates_stable_identity_from_content() -> Result<(), DocsightError> {
        let first = DocumentSource::from_bytes(b"%PDF-1.7\nstable".to_vec())?;
        let second = DocumentSource::from_bytes(b"%PDF-1.7\nstable".to_vec())?;
        assert_eq!(first.id(), second.id());
        assert_eq!(first.sha256(), second.sha256());
        Ok(())
    }

    #[test]
    fn validates_rectangles_and_intersections() -> Result<(), DocsightError> {
        let first = Rect::new(0.0, 0.0, 20.0, 10.0)?;
        let second = Rect::new(10.0, 5.0, 30.0, 15.0)?;
        assert_eq!(first.width(), 20.0);
        assert_eq!(first.height(), 10.0);
        let expected = Rect::new(10.0, 5.0, 20.0, 10.0)?;
        assert_eq!(first.intersection(second), Some(expected));
        assert!(Rect::new(0.0, 0.0, 0.0, 1.0).is_err());
        assert!(Rect::new(f32::NAN, 0.0, 1.0, 1.0).is_err());
        Ok(())
    }

    #[test]
    fn maps_m2_errors_to_stable_exit_codes() {
        assert_eq!(
            DocsightError::InvalidArgument {
                message: "invalid".to_owned()
            }
            .exit_code(),
            2
        );
        assert_eq!(DocsightError::EncryptedDocument.exit_code(), 12);
        assert_eq!(
            DocsightError::UnsupportedFeature {
                feature: "feature".to_owned()
            }
            .exit_code(),
            20
        );
        assert_eq!(
            DocsightError::BackendFailure {
                backend: "backend".to_owned(),
                message: "failure".to_owned()
            }
            .exit_code(),
            30
        );
    }
}
