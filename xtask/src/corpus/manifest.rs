use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub file: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub exit_code: u16,
    pub diagnostic_codes: Vec<String>,
    pub pointer_equals: BTreeMap<String, Value>,
    pub repeat: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub file: String,
    pub sha256: String,
    pub origin: String,
    pub format: String,
    pub operation: String,
    pub expected: Expectation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<Input>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub cases: Vec<Case>,
}

pub fn checked_input(file: &str, sha: &str, root: Option<&Path>) -> Result<()> {
    safe_member(file)?;
    checked_digest(sha)?;
    if let Some(root) = root {
        let path = contained_file(root, file)?;
        let bytes = read_bytes(&path, MAX_FILE_BYTES)?;
        require(
            digest(&bytes) == sha,
            "CORPUS_DIGEST_MISMATCH",
            "Corpus input differs from its reviewed digest",
        )?;
    }
    Ok(())
}

pub fn checked_pointer(pointer: &str) -> Result<()> {
    require(
        pointer.starts_with('/') && pointer.len() <= 512,
        "INVALID_CORPUS_ASSERTIONS",
        "Assertions require bounded JSON pointers",
    )?;
    let mut characters = pointer.chars();
    while let Some(character) = characters.next() {
        if character == '~' {
            require(
                matches!(characters.next(), Some('0' | '1')),
                "INVALID_CORPUS_ASSERTIONS",
                "Invalid JSON pointer escape",
            )?;
        }
    }
    Ok(())
}

pub fn validate_manifest(manifest: Manifest, root: Option<&Path>) -> Result<Manifest> {
    require(
        manifest.schema == "docsight.corpus/v1" && (1..=10_000).contains(&manifest.cases.len()),
        "INVALID_CORPUS_MANIFEST",
        "A bounded versioned corpus manifest is required",
    )?;
    let mut seen = BTreeSet::new();
    for case in &manifest.cases {
        let valid_id = (1..=80).contains(&case.id.len())
            && case
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !case.id.starts_with('-');
        require(
            valid_id && seen.insert(&case.id),
            "INVALID_CORPUS_ID",
            "Case identifiers must be unique portable slugs",
        )?;
        require(
            super::ORIGINS.contains(&case.origin.as_str())
                && super::FORMATS.contains(&case.format.as_str())
                && super::OPERATIONS.contains(&case.operation.as_str()),
            "INVALID_CORPUS_VALUE",
            "Unsupported corpus origin, format or operation",
        )?;
        checked_input(&case.file, &case.sha256, root)?;
        if let Some(reference) = &case.reference {
            require(
                case.operation == "diff",
                "UNUSED_CORPUS_REFERENCE",
                "Only diff accepts a reference document",
            )?;
            checked_input(&reference.file, &reference.sha256, root)?;
        }
        let expected = &case.expected;
        require(
            expected.exit_code <= 255 && (1..=3).contains(&expected.repeat),
            "INVALID_INTEGER",
            "Expected exit code or repeat count is outside its limits",
        )?;
        require(
            expected.diagnostic_codes.len() <= 128
                && expected.diagnostic_codes.iter().all(|code| is_code(code))
                && expected
                    .diagnostic_codes
                    .windows(2)
                    .all(|pair| pair[0] < pair[1]),
            "INVALID_CORPUS_DIAGNOSTICS",
            "Diagnostics must be sorted unique public codes",
        )?;
        require(
            expected.exit_code == 0 || !expected.diagnostic_codes.is_empty(),
            "MISSING_CORPUS_ERROR",
            "Negative cases require typed diagnostics",
        )?;
        require(
            expected.pointer_equals.len() <= 64,
            "INVALID_CORPUS_ASSERTIONS",
            "Too many corpus assertions",
        )?;
        for (pointer, value) in &expected.pointer_equals {
            checked_pointer(pointer)?;
            require(
                !value.is_array() && !value.is_object(),
                "INVALID_CORPUS_ASSERTIONS",
                "Assertions must compare scalar JSON values",
            )?;
        }
    }
    Ok(manifest)
}

pub fn load_manifest(path: &Path, root: Option<&Path>) -> Result<Manifest> {
    validate_manifest(decode(read_json(path)?)?, root)
}
