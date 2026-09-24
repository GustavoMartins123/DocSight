use super::manifest::{
    Case, Expectation, Manifest, checked_input, checked_pointer, validate_manifest,
};
use crate::quality::Taxonomy;
use crate::release::{
    archive::{extract_verified, verify_archive},
    native_target,
};
use crate::smoke::validate_png_bytes;
use crate::tooling::{
    common::*,
    process::{NativeRunner, ProcessLimits, ProcessResult, Runner, isolated_environment},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseOutcome {
    pub id: String,
    pub origin: String,
    pub format: String,
    pub class: String,
    pub document_sha256: String,
    #[serde(deserialize_with = "required_option")]
    pub reference_sha256: Option<String>,
    pub operation: String,
    pub passed: bool,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<String>,
    pub attempts: u32,
    pub elapsed_ms: u64,
    #[serde(deserialize_with = "required_option")]
    pub output_sha256: Option<String>,
    pub diagnostic_codes: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: String,
    pub version: String,
    pub revision: String,
    pub target: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub cases: Vec<CaseOutcome>,
    pub passed: bool,
}

pub fn json_pointer<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    checked_pointer(pointer)?;
    value.pointer(pointer).ok_or_else(|| {
        ToolError::new(
            "CORPUS_MISSING_POINTER",
            "Expected JSON pointer does not exist",
        )
    })
}

pub fn evaluate(result: &ProcessResult, expected: &Expectation) -> Result<Vec<String>> {
    require(
        result.termination.is_none(),
        "CORPUS_PROCESS_LIMIT",
        "Corpus command exceeded its execution budget",
    )?;
    require(
        (0..=65535).contains(&result.returncode),
        "CORPUS_CRASH",
        "Corpus command terminated abnormally",
    )?;
    require(
        result.returncode == i64::from(expected.exit_code),
        "CORPUS_EXIT_CODE",
        "Unexpected corpus exit code",
    )?;
    let value = parse_json(if result.returncode == 0 {
        &result.stdout
    } else {
        &result.stderr
    })?;
    require(
        value.get("schema").and_then(Value::as_str) == Some("docsight.agent/v2"),
        "CORPUS_PROTOCOL",
        "Expected the versioned agent envelope",
    )?;
    if result.returncode == 0 {
        require(
            result.stderr.is_empty() && value.get("result").is_some_and(Value::is_object),
            "CORPUS_PROTOCOL",
            "Successful engine output is not a clean result envelope",
        )?;
    } else {
        require(
            result.stdout.is_empty()
                && value.get("error").is_some_and(Value::is_object)
                && value.pointer("/error/exit_code").and_then(Value::as_i64)
                    == Some(result.returncode),
            "CORPUS_PROTOCOL",
            "Error output is not a clean typed error envelope",
        )?;
    }
    let mut records: Vec<&Value> = match value.get("warnings") {
        Some(warnings) => array(warnings)?.iter().collect(),
        None => Vec::new(),
    };
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        records.push(error);
    }
    let codes: BTreeSet<_> = records
        .iter()
        .filter_map(|record| record.get("code").and_then(Value::as_str))
        .collect();
    require(
        expected
            .diagnostic_codes
            .iter()
            .all(|code| codes.contains(code.as_str())),
        "CORPUS_DIAGNOSTIC",
        "A required diagnostic is missing",
    )?;
    for (pointer, wanted) in &expected.pointer_equals {
        let actual = json_pointer(&value, pointer)?;
        let same_number_type = match (actual, wanted) {
            (Value::Number(a), Value::Number(b)) => a.is_f64() == b.is_f64(),
            _ => true,
        };
        require(
            actual == wanted && same_number_type,
            "CORPUS_ASSERTION",
            "A reviewed result assertion failed",
        )?;
    }
    Ok(expected.diagnostic_codes.clone())
}

fn verify_input(case: &Case, root: &Path) -> Result<()> {
    checked_input(&case.file, &case.sha256, Some(root))?;
    if let Some(reference) = &case.reference {
        checked_input(&reference.file, &reference.sha256, Some(root))?;
    }
    Ok(())
}

fn run_case<R: Runner>(
    case: &Case,
    root: &Path,
    binary: &Path,
    work: &Path,
    runner: &mut R,
) -> CaseOutcome {
    let mut outcome = CaseOutcome {
        id: case.id.clone(),
        origin: case.origin.clone(),
        format: case.format.clone(),
        class: case.class.clone(),
        document_sha256: case.sha256.clone(),
        reference_sha256: case.reference.as_ref().map(|input| input.sha256.clone()),
        operation: case.operation.clone(),
        passed: false,
        error_code: None,
        attempts: 0,
        elapsed_ms: 0,
        output_sha256: None,
        diagnostic_codes: Vec::new(),
    };
    let operation = (|| -> Result<()> {
        let environment = isolated_environment()?;
        let output = work.join(format!("{}.png", case.id));
        let document = contained_file(root, &case.file)?;
        let reference = match &case.reference {
            Some(reference) => contained_file(root, &reference.file)?,
            None => document.clone(),
        };
        let mut args: Vec<OsString> = vec![
            binary.as_os_str().to_owned(),
            "--agent".into(),
            "--sandbox".into(),
            case.operation.clone().into(),
            document.into_os_string(),
        ];
        if case.operation == "render" {
            args.extend([
                "--page".into(),
                "1".into(),
                "--dpi".into(),
                "72".into(),
                "--out".into(),
                output.as_os_str().to_owned(),
            ]);
        }
        if case.operation == "diff" {
            args.push(reference.into_os_string());
        }
        for _ in 0..case.expected.repeat {
            verify_input(case, root)?;
            if output.try_exists()? {
                fs::remove_file(&output)?;
            }
            let result = runner.run(
                &args,
                work,
                &ProcessLimits {
                    timeout: Duration::from_secs(45),
                    output_bytes: 8_388_608,
                },
                Some(&environment),
            )?;
            outcome.attempts += 1;
            outcome.elapsed_ms = outcome
                .elapsed_ms
                .checked_add(result.elapsed_ms)
                .ok_or_else(|| ToolError::new("INVALID_NUMBER", "Corpus duration overflow"))?;
            verify_input(case, root)?;
            outcome.diagnostic_codes = evaluate(&result, &case.expected)?;
            let mut hash = Sha256::new();
            hash.update(&result.stdout);
            hash.update([0]);
            hash.update(&result.stderr);
            if case.operation == "render" && result.returncode == 0 {
                let bytes = read_bytes(&output, 67_108_864)?;
                validate_png_bytes(&bytes)?;
                hash.update(Sha256::digest(&bytes));
            }
            let current = lowercase_hex(&hash.finalize());
            require(
                outcome
                    .output_sha256
                    .as_ref()
                    .is_none_or(|old| *old == current),
                "CORPUS_NONDETERMINISTIC",
                "Repeated command changed output bytes",
            )?;
            outcome.output_sha256 = Some(current);
        }
        Ok(())
    })();
    outcome.passed = operation.is_ok();
    outcome.error_code = operation.err().map(|error| error.code.into());
    outcome
}

pub fn run_corpus_with<R: Runner>(
    archive: &Path,
    manifest_path: &Path,
    root: &Path,
    taxonomy: &Taxonomy,
    runner: &mut R,
) -> Result<Report> {
    let manifest_bytes = read_bytes(manifest_path, MAX_JSON_BYTES)?;
    let corpus: Manifest =
        validate_manifest(decode(parse_json(&manifest_bytes)?)?, Some(root), taxonomy)?;
    let manifest_hash = digest(&manifest_bytes);
    let manifest = verify_archive(archive)?;
    require(
        manifest.target == native_target()?,
        "CORPUS_HOST_MISMATCH",
        "Corpus execution requires a native archive",
    )?;
    let archive_hash = sha256_file(archive)?;
    let temporary = tempfile::tempdir()?;
    let temporary_root = temporary.path().canonicalize()?;
    let (binary, manifest) = extract_verified(archive, &temporary_root.join("package"))?;
    let work = temporary_root.join("work");
    fs::create_dir(&work)?;
    let version = runner.run(
        &[binary.as_os_str().to_owned(), "--version".into()],
        &work,
        &ProcessLimits {
            timeout: Duration::from_secs(10),
            output_bytes: 4096,
        },
        Some(&isolated_environment()?),
    )?;
    require(
        version.termination.is_none()
            && version.returncode == 0
            && version.stderr.is_empty()
            && text(&version.stdout)?
                .trim()
                .starts_with(&format!("docsight {}", manifest.version)),
        "CORPUS_VERSION_MISMATCH",
        "Executable version differs from archive manifest",
    )?;
    let cases: Vec<_> = corpus
        .cases
        .iter()
        .map(|case| run_case(case, root, &binary, &work, runner))
        .collect();
    require(
        sha256_file(manifest_path)? == manifest_hash && sha256_file(archive)? == archive_hash,
        "CORPUS_EVIDENCE_CHANGED",
        "Archive or corpus manifest changed during execution",
    )?;
    Ok(Report {
        schema: "docsight.corpus-report/v1".into(),
        version: manifest.version,
        revision: manifest.revision,
        target: manifest.target,
        archive_sha256: archive_hash,
        manifest_sha256: manifest_hash,
        passed: cases.iter().all(|case| case.passed),
        cases,
    })
}

pub fn run_corpus(
    archive: &Path,
    manifest_path: &Path,
    root: &Path,
    taxonomy: &Taxonomy,
) -> Result<Report> {
    run_corpus_with(archive, manifest_path, root, taxonomy, &mut NativeRunner)
}
