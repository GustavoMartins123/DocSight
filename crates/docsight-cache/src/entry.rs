use crate::key::{CacheKey, hex, is_sha256};
use docsight_core::{DocsightError, Document, IrVersion, validate_canonical};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ENTRY_SCHEMA: &str = "docsight.cache-entry/v1";
const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryHeader {
    schema: String,
    key: CacheKey,
    payload_sha256: String,
    payload_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryRejection {
    MissingHeader,
    InvalidHeader,
    KeyMismatch,
    PayloadLength,
    PayloadDigest,
    InvalidPayload,
    DocumentIdentity,
    IrVersion,
    NonCanonical,
}

impl EntryRejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing_header",
            Self::InvalidHeader => "invalid_header",
            Self::KeyMismatch => "key_mismatch",
            Self::PayloadLength => "payload_length",
            Self::PayloadDigest => "payload_digest",
            Self::InvalidPayload => "invalid_payload",
            Self::DocumentIdentity => "document_identity",
            Self::IrVersion => "ir_version",
            Self::NonCanonical => "non_canonical",
        }
    }
}

pub fn encode_entry(key: &CacheKey, document: &Document) -> Result<Vec<u8>, DocsightError> {
    let payload = serde_json::to_vec(document).map_err(serialization_failure)?;
    let header = EntryHeader {
        schema: ENTRY_SCHEMA.to_owned(),
        key: key.clone(),
        payload_sha256: hex(&Sha256::digest(&payload)),
        payload_bytes: payload.len() as u64,
    };
    let mut bytes = serde_json::to_vec(&header).map_err(serialization_failure)?;
    bytes.push(b'\n');
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub fn decode_entry(bytes: &[u8], expected: &CacheKey) -> Result<Document, EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if header.key != *expected {
        return Err(EntryRejection::KeyMismatch);
    }
    decode_payload(&header, payload)
}

pub(crate) fn verify_entry_integrity(bytes: &[u8]) -> Result<CacheKey, EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if !header.key.is_well_formed() {
        return Err(EntryRejection::InvalidHeader);
    }
    verify_payload_bytes(&header, payload)?;
    Ok(header.key)
}

fn split_entry(bytes: &[u8]) -> Result<(EntryHeader, &[u8]), EntryRejection> {
    let newline = bytes
        .iter()
        .take(MAX_HEADER_BYTES)
        .position(|byte| *byte == b'\n')
        .ok_or(EntryRejection::MissingHeader)?;
    let header: EntryHeader =
        serde_json::from_slice(&bytes[..newline]).map_err(|_| EntryRejection::InvalidHeader)?;
    if header.schema != ENTRY_SCHEMA || !is_sha256(&header.payload_sha256) {
        return Err(EntryRejection::InvalidHeader);
    }
    Ok((header, &bytes[newline + 1..]))
}

fn verify_payload_bytes(header: &EntryHeader, payload: &[u8]) -> Result<(), EntryRejection> {
    if payload.len() as u64 != header.payload_bytes {
        return Err(EntryRejection::PayloadLength);
    }
    if hex(&Sha256::digest(payload)) != header.payload_sha256 {
        return Err(EntryRejection::PayloadDigest);
    }
    Ok(())
}

fn decode_payload(header: &EntryHeader, payload: &[u8]) -> Result<Document, EntryRejection> {
    verify_payload_bytes(header, payload)?;
    let document: Document =
        serde_json::from_slice(payload).map_err(|_| EntryRejection::InvalidPayload)?;
    if document.sha256 != header.key.document_sha256
        || document.format != header.key.document_format
    {
        return Err(EntryRejection::DocumentIdentity);
    }
    if document.version != IrVersion::current()
        || header.key.engine.ir_schema_version != document.version.schema_version
    {
        return Err(EntryRejection::IrVersion);
    }
    validate_canonical(&document).map_err(|_| EntryRejection::NonCanonical)?;
    Ok(document)
}

fn serialization_failure(error: serde_json::Error) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "docsight-cache".to_owned(),
        message: format!("cache entry could not be serialized: {error}"),
    }
}
