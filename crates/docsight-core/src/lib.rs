pub mod canonical;
pub mod evidence;
mod glyph;
pub mod ir;
pub mod object;
pub mod png;

pub use canonical::*;
pub use evidence::*;
pub use glyph::*;
pub use ir::*;
pub use object::*;
pub use png::*;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

impl Diagnostic {
    pub fn warning(code: &str, message: String, effect: &str) -> Self {
        Self {
            code: code.to_owned(),
            severity: DiagnosticSeverity::Warning,
            message,
            effect: effect.to_owned(),
            object: None,
            page: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

impl Display for ErrorLocation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let mut fields = Vec::new();
        if let Some(page) = self.page {
            fields.push(format!("page {page}"));
        }
        if let Some(object) = &self.object {
            fields.push(format!("object {object}"));
        }
        if let Some(operator) = &self.operator {
            fields.push(format!("operator {operator}"));
        }
        if let Some(offset) = self.offset {
            fields.push(format!("offset {offset}"));
        }
        formatter.write_str(&fields.join(", "))
    }
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
    #[error("malformed document at {location}: {message}")]
    MalformedDocumentAt {
        message: String,
        location: ErrorLocation,
    },
    #[error("verification failed: {message}")]
    VerificationFailed { message: String },
    #[error("encrypted documents require a password and are not supported")]
    EncryptedDocument,
    #[error("encrypted OOXML package detected inside an OLE2 container")]
    EncryptedPackage,
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
            Self::MalformedDocument { .. }
            | Self::MalformedDocumentAt { .. }
            | Self::VerificationFailed { .. } => 11,
            Self::EncryptedDocument | Self::EncryptedPackage => 12,
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
                "LAYOUT_PARTIAL",
                "the requested result requires unsupported layout or rendering behavior",
            ),
            Self::MalformedDocument { .. } | Self::MalformedDocumentAt { .. } => (
                "MALFORMED_DOCUMENT",
                "the document was rejected during parsing",
            ),
            Self::VerificationFailed { .. } => (
                "VERIFICATION_FAILED",
                "the artifact did not reproduce its claimed evidence",
            ),
            Self::EncryptedDocument => ("ENCRYPTED", "the document was rejected before inspection"),
            Self::EncryptedPackage => ("ENCRYPTED", "the document was rejected before inspection"),
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
        let page = self.error_location().and_then(|location| location.page);
        Diagnostic {
            code: code.to_owned(),
            severity: DiagnosticSeverity::Error,
            message: self.to_string(),
            effect: effect.to_owned(),
            object: None,
            page,
        }
    }

    pub fn with_error_location(self, location: ErrorLocation) -> Self {
        match self {
            Self::MalformedDocument { message } => Self::MalformedDocumentAt { message, location },
            Self::MalformedDocumentAt {
                message,
                location: existing,
            } => Self::MalformedDocumentAt {
                message,
                location: ErrorLocation {
                    page: location.page.or(existing.page),
                    object: location.object.or(existing.object),
                    operator: location.operator.or(existing.operator),
                    offset: location.offset.or(existing.offset),
                },
            },
            error => error,
        }
    }

    pub fn error_location(&self) -> Option<&ErrorLocation> {
        match self {
            Self::MalformedDocumentAt { location, .. } => Some(location),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorCatalogEntry {
    pub code: &'static str,
    pub exit_code: u8,
    pub meaning: &'static str,
}

pub const SUCCESS_EXIT_CODE: u8 = 0;

pub const ERROR_CATALOG: &[ErrorCatalogEntry] = &[
    ErrorCatalogEntry {
        code: "USAGE",
        exit_code: 2,
        meaning: "the invocation or its arguments were rejected",
    },
    ErrorCatalogEntry {
        code: "UNSUPPORTED_FORMAT",
        exit_code: 10,
        meaning: "the document format, or the requested operation for that format, is not supported",
    },
    ErrorCatalogEntry {
        code: "MALFORMED_DOCUMENT",
        exit_code: 11,
        meaning: "the document was rejected during parsing",
    },
    ErrorCatalogEntry {
        code: "VERIFICATION_FAILED",
        exit_code: 11,
        meaning: "an artifact did not reproduce its claimed evidence",
    },
    ErrorCatalogEntry {
        code: "ENCRYPTED",
        exit_code: 12,
        meaning: "the document is encrypted and was rejected before inspection",
    },
    ErrorCatalogEntry {
        code: "RESOURCE_LIMIT",
        exit_code: 13,
        meaning: "a declared resource limit was exceeded",
    },
    ErrorCatalogEntry {
        code: "LAYOUT_PARTIAL",
        exit_code: 20,
        meaning: "the result requires layout or rendering behavior that is not supported",
    },
    ErrorCatalogEntry {
        code: "OBJECT_NOT_FOUND",
        exit_code: 21,
        meaning: "the requested object id does not exist in the document",
    },
    ErrorCatalogEntry {
        code: "BACKEND_FAILURE",
        exit_code: 30,
        meaning: "an internal backend could not complete the operation",
    },
    ErrorCatalogEntry {
        code: "IO_ERROR",
        exit_code: 40,
        meaning: "the file could not be read",
    },
];

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
        format!("doc_{}", &self.digest[..32])
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
        let suffix: String = digest[..16]
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

    pub fn contains_point(self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn intersects(self, other: Self) -> bool {
        self.intersection(other).is_some()
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
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        if contains_utf16le(bytes, "EncryptedPackage") {
            return Err(DocsightError::EncryptedPackage);
        }
        return Err(DocsightError::UnsupportedFormat);
    }
    Err(DocsightError::UnsupportedFormat)
}

fn contains_utf16le(haystack: &[u8], needle: &str) -> bool {
    let encoded: Vec<u8> = needle
        .chars()
        .flat_map(|character| {
            let code = character as u32;
            vec![(code & 0xFF) as u8, ((code >> 8) & 0xFF) as u8]
        })
        .collect();
    contains_bytes(haystack, &encoded)
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
    use super::{
        DocsightError, DocumentFormat, DocumentSource, ERROR_CATALOG, ErrorLocation, Rect,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn docx_signature() -> Vec<u8> {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(b"[Content_Types].xml");
        bytes.extend_from_slice(b"word/document.xml");
        bytes
    }

    fn variant_name(error: &DocsightError) -> &'static str {
        match error {
            DocsightError::InvalidArgument { .. } => "InvalidArgument",
            DocsightError::Io { .. } => "Io",
            DocsightError::UnsupportedFormat => "UnsupportedFormat",
            DocsightError::ResourceLimit { .. } => "ResourceLimit",
            DocsightError::MalformedDocument { .. } => "MalformedDocument",
            DocsightError::MalformedDocumentAt { .. } => "MalformedDocumentAt",
            DocsightError::VerificationFailed { .. } => "VerificationFailed",
            DocsightError::EncryptedDocument => "EncryptedDocument",
            DocsightError::EncryptedPackage => "EncryptedPackage",
            DocsightError::BackendFailure { .. } => "BackendFailure",
            DocsightError::UnsupportedOperation { .. } => "UnsupportedOperation",
            DocsightError::UnsupportedFeature { .. } => "UnsupportedFeature",
            DocsightError::ObjectNotFound { .. } => "ObjectNotFound",
        }
    }

    fn every_error_variant() -> Vec<DocsightError> {
        vec![
            DocsightError::InvalidArgument {
                message: "message".to_owned(),
            },
            DocsightError::Io {
                path: PathBuf::from("document.pdf"),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
            DocsightError::UnsupportedFormat,
            DocsightError::ResourceLimit {
                resource: "bytes".to_owned(),
                limit: 1,
            },
            DocsightError::MalformedDocument {
                message: "message".to_owned(),
            },
            DocsightError::MalformedDocumentAt {
                message: "message".to_owned(),
                location: ErrorLocation::default(),
            },
            DocsightError::VerificationFailed {
                message: "message".to_owned(),
            },
            DocsightError::EncryptedDocument,
            DocsightError::EncryptedPackage,
            DocsightError::BackendFailure {
                backend: "backend".to_owned(),
                message: "message".to_owned(),
            },
            DocsightError::UnsupportedOperation {
                operation: "render".to_owned(),
                format: DocumentFormat::Docx,
            },
            DocsightError::UnsupportedFeature {
                feature: "feature".to_owned(),
            },
            DocsightError::ObjectNotFound {
                object: "p_0".to_owned(),
            },
        ]
    }

    #[test]
    fn error_catalog_is_complete() {
        let samples = every_error_variant();
        let covered = samples.iter().map(variant_name).collect::<BTreeSet<_>>();
        assert_eq!(
            covered.len(),
            samples.len(),
            "every_error_variant must list each variant exactly once"
        );

        let mut reached = BTreeSet::new();
        let mut missing = Vec::new();
        for error in &samples {
            let diagnostic = error.diagnostic();
            match ERROR_CATALOG
                .iter()
                .find(|entry| entry.code == diagnostic.code)
            {
                Some(entry) => {
                    assert_eq!(
                        entry.exit_code,
                        error.exit_code(),
                        "{} disagrees with ERROR_CATALOG on the exit code for {}",
                        variant_name(error),
                        diagnostic.code
                    );
                    reached.insert(entry.code);
                }
                None => missing.push(format!("{} -> {}", variant_name(error), diagnostic.code)),
            }
        }
        assert!(
            missing.is_empty(),
            "ERROR_CATALOG is missing codes produced by: {}",
            missing.join(", ")
        );

        let listed = ERROR_CATALOG
            .iter()
            .map(|entry| entry.code)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            listed, reached,
            "ERROR_CATALOG lists codes that no DocsightError variant can produce"
        );
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
    fn identifies_encrypted_ooxml_package_inside_ole2_container() {
        let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        for character in "EncryptedPackage".chars() {
            let code = character as u32;
            bytes.push((code & 0xFF) as u8);
            bytes.push(((code >> 8) & 0xFF) as u8);
        }
        let error = DocumentSource::from_bytes(bytes);
        match error {
            Err(error @ DocsightError::EncryptedPackage) => assert_eq!(error.exit_code(), 12),
            _ => unreachable!("expected EncryptedPackage"),
        }
    }

    #[test]
    fn rejects_legacy_ole2_documents_as_unsupported_format() {
        let bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00, 0x01];
        let error = DocumentSource::from_bytes(bytes);
        match error {
            Err(error @ DocsightError::UnsupportedFormat) => assert_eq!(error.exit_code(), 10),
            _ => unreachable!("expected UnsupportedFormat"),
        }
    }

    #[test]
    fn maps_exit_twenty_to_layout_partial_code() {
        let diagnostic = DocsightError::UnsupportedFeature {
            feature: "FlateDecode".to_owned(),
        }
        .diagnostic();
        assert_eq!(diagnostic.code, "LAYOUT_PARTIAL");
        assert_eq!(
            DocsightError::UnsupportedFeature {
                feature: "FlateDecode".to_owned()
            }
            .exit_code(),
            20
        );
    }

    #[test]
    fn maps_encrypted_package_to_encrypted_code() {
        let diagnostic = DocsightError::EncryptedPackage.diagnostic();
        assert_eq!(diagnostic.code, "ENCRYPTED");
        assert_eq!(DocsightError::EncryptedPackage.exit_code(), 12);
    }

    #[test]
    fn creates_stable_identity_from_content() -> Result<(), DocsightError> {
        let first = DocumentSource::from_bytes(b"%PDF-1.7\nstable".to_vec())?;
        let second = DocumentSource::from_bytes(b"%PDF-1.7\nstable".to_vec())?;
        assert_eq!(first.id(), second.id());
        assert_eq!(first.sha256(), second.sha256());
        assert_eq!(first.id().len(), 36);
        assert_eq!(first.object_id("p", "page[1]").as_str().len(), 34);
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

    #[test]
    fn maps_verification_failure_to_a_typed_artifact_error() {
        let error = DocsightError::VerificationFailed {
            message: "mismatch".to_owned(),
        };
        assert_eq!(error.exit_code(), 11);
        assert_eq!(error.diagnostic().code, "VERIFICATION_FAILED");
    }
}
