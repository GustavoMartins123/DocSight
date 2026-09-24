use crate::key::{CacheKey, hex, is_sha256};
use docsight_core::{DocsightError, Document, IrVersion, validate_canonical};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ENTRY_SCHEMA: &str = "docsight.cache-entry/v1";
const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryProducer {
    InProcess,
    SandboxWorker,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryHeader {
    schema: String,
    key: CacheKey,
    producer: EntryProducer,
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

pub fn encode_entry(
    key: &CacheKey,
    document: &Document,
    producer: EntryProducer,
) -> Result<Vec<u8>, DocsightError> {
    if document.sha256 != key.document_sha256
        || document.format != key.document_format
        || document.version != IrVersion::current()
        || key.engine.ir_schema_version != document.version.schema_version
    {
        return Err(write_rejected(
            "the document IR does not match the cache key".to_owned(),
        ));
    }
    validate_canonical(document).map_err(|error| write_rejected(error.to_string()))?;
    let payload = serde_json::to_vec(document).map_err(serialization_failure)?;
    let header = EntryHeader {
        schema: ENTRY_SCHEMA.to_owned(),
        key: key.clone(),
        producer,
        payload_sha256: hex(&Sha256::digest(&payload)),
        payload_bytes: payload.len() as u64,
    };
    let mut bytes = serde_json::to_vec(&header).map_err(serialization_failure)?;
    bytes.push(b'\n');
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub fn verify_entry(bytes: &[u8], expected: &CacheKey) -> Result<EntryProducer, EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if header.key != *expected {
        return Err(EntryRejection::KeyMismatch);
    }
    verify_payload_bytes(&header, payload)?;
    Ok(header.producer)
}

pub fn decode_entry(bytes: &[u8], expected: &CacheKey) -> Result<Document, EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if header.key != *expected {
        return Err(EntryRejection::KeyMismatch);
    }
    verify_payload_bytes(&header, payload)?;
    decode_payload(&header, payload)
}

pub fn decode_untrusted_entry(
    bytes: &[u8],
    expected: &CacheKey,
) -> Result<Document, EntryRejection> {
    let document = decode_entry(bytes, expected)?;
    validate_canonical(&document).map_err(|_| EntryRejection::NonCanonical)?;
    Ok(document)
}

pub(crate) fn verify_and_decode_untrusted_entry(
    bytes: &[u8],
    expected: &CacheKey,
) -> Result<Document, EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if header.key != *expected {
        return Err(EntryRejection::KeyMismatch);
    }
    let document = decode_payload(&header, payload)?;
    validate_canonical(&document).map_err(|_| EntryRejection::NonCanonical)?;
    Ok(document)
}

pub(crate) fn verify_entry_integrity(
    bytes: &[u8],
) -> Result<(CacheKey, EntryProducer), EntryRejection> {
    let (header, payload) = split_entry(bytes)?;
    if !header.key.is_well_formed() {
        return Err(EntryRejection::InvalidHeader);
    }
    verify_payload_bytes(&header, payload)?;
    Ok((header.key, header.producer))
}

pub(crate) fn probe_header(bytes: &[u8]) -> Option<(CacheKey, EntryProducer)> {
    split_entry(bytes)
        .ok()
        .map(|(header, _)| (header.key, header.producer))
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
    Ok(document)
}

fn write_rejected(reason: String) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "docsight-cache".to_owned(),
        message: format!("the document IR was not written to the cache: {reason}"),
    }
}

fn serialization_failure(error: serde_json::Error) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "docsight-cache".to_owned(),
        message: format!("cache entry could not be serialized: {error}"),
    }
}
