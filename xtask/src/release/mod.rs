pub mod archive;
pub mod binary;
pub mod changelog;
pub mod notices;
pub mod signature;

use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const TARGETS: [&str; 5] = [
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];
pub const DOCUMENTS: [&str; 13] = [
    "README.md",
    "INSTALL.md",
    "PRODUCT_SCOPE.md",
    "AGENT_PROTOCOL.md",
    "PERFORMANCE.md",
    "CHANGELOG.md",
    "BACKLOG.md",
    "FUZZING.md",
    "BETA.md",
    "RELEASE.md",
    "TOOLING.md",
    "LICENSE-MIT",
    "LICENSE-APACHE",
];
pub const EXAMPLES: [&str; 2] = ["sample_headings.docx", "sample_semantic.pdf"];
pub const SMOKE_CHECKS: [&str; 18] = [
    "version",
    "capabilities",
    "docx_inspect",
    "pdf_inspect",
    "docx_determinism",
    "pdf_determinism",
    "docx_text",
    "pdf_text",
    "docx_render",
    "pdf_render",
    "identical_diff",
    "sandbox_inspect",
    "typed_error",
    "completion_bash",
    "completion_elvish",
    "completion_fish",
    "completion_powershell",
    "completion_zsh",
];

pub fn checked_target(target: &str) -> Result<()> {
    require(
        TARGETS.contains(&target),
        "UNSUPPORTED_TARGET",
        "Target is not part of the five-platform release matrix",
    )
}

pub fn binary_names(target: &str) -> Result<(&'static str, &'static str)> {
    checked_target(target)?;
    if target == "x86_64-pc-windows-msvc" {
        Ok(("docsight.exe", "docsight-worker.exe"))
    } else {
        Ok(("docsight", "docsight-worker"))
    }
}

pub fn basename(version: &str, target: &str) -> Result<String> {
    checked_version(version)?;
    checked_target(target)?;
    Ok(format!("docsight-{version}-{target}"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixEntry {
    pub target: String,
    pub runner: String,
    pub binary: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matrix {
    pub include: Vec<MatrixEntry>,
}

pub fn matrix(root: &Path) -> Result<Matrix> {
    let matrix: Matrix = decode(read_json(&root.join("release/targets.json"))?)?;
    let mut seen = BTreeSet::new();
    for entry in &matrix.include {
        let (binary, _) = binary_names(&entry.target)?;
        require(
            seen.insert(entry.target.as_str()) && entry.binary == binary,
            "INVALID_MATRIX",
            "Matrix contains a duplicate or mismatched target",
        )?;
        require(
            !entry.runner.is_empty()
                && entry.runner.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte)
                }),
            "INVALID_RUNNER",
            "Runner label must be a literal platform label",
        )?;
    }
    require(
        seen == TARGETS.into_iter().collect(),
        "INCOMPLETE_MATRIX",
        "All five release targets are required",
    )?;
    Ok(matrix)
}

pub fn native_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(TARGETS[0]),
        ("linux", "x86_64") => Ok(TARGETS[1]),
        ("linux", "aarch64") => Ok(TARGETS[2]),
        ("macos", "x86_64") => Ok(TARGETS[3]),
        ("macos", "aarch64") => Ok(TARGETS[4]),
        _ => Err(ToolError::new(
            "UNSUPPORTED_HOST",
            "Native checks require a supported operating system and architecture",
        )),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<String>,
    pub elapsed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmokeReceipt {
    pub schema: String,
    pub version: String,
    pub target: String,
    pub revision: String,
    pub archive_sha256: String,
    pub checks: Vec<Check>,
    pub passed: bool,
}

pub fn validate_smoke_receipt(
    receipt: &SmokeReceipt,
    manifest: &archive::Manifest,
    hash: &str,
) -> Result<()> {
    require(
        receipt.schema == "docsight.release-smoke/v1"
            && receipt.passed
            && receipt.archive_sha256 == hash
            && receipt.version == manifest.version
            && receipt.target == manifest.target
            && receipt.revision == manifest.revision,
        "INVALID_SMOKE_RECEIPT",
        "Smoke receipt does not attest this exact candidate archive",
    )?;
    require(
        receipt.checks.len() == SMOKE_CHECKS.len(),
        "INCOMPLETE_SMOKE_RECEIPT",
        "Every native smoke check is required",
    )?;
    for (check, name) in receipt.checks.iter().zip(SMOKE_CHECKS) {
        require(
            check.name == name
                && check.passed
                && check.error_code.is_none()
                && check.elapsed_ms <= 3_600_000,
            "FAILED_SMOKE_RECEIPT",
            "Every native check must pass exactly once in canonical order",
        )?;
    }
    Ok(())
}
