use crate::release::{
    archive::{extract_verified, verify_archive},
    checked_target, native_target,
};
use crate::tooling::{
    common::*,
    process::{
        NativeRunner, ProcessLimits, ProcessResult, Runner, Termination, isolated_environment,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const OPERATIONS: [&str; 6] = [
    "capabilities",
    "inspect",
    "overview",
    "text",
    "render",
    "diff",
];
pub const EXPERIENCES: [&str; 3] = ["clear", "confusing", "blocked"];
pub const OUTCOMES: [&str; 6] = [
    "success",
    "error",
    "crash",
    "timeout",
    "output_limit",
    "invalid_protocol",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: String,
    pub version: String,
    pub revision: String,
    pub target: String,
    pub archive_sha256: String,
    pub participant: String,
    pub operation: String,
    pub experience: String,
    pub outcome: String,
    pub exit_code: i64,
    pub elapsed_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub document_size_bytes: u64,
    #[serde(deserialize_with = "required_option")]
    pub document_sha256: Option<String>,
    pub diagnostic_codes: Vec<String>,
    pub unknown_diagnostic_count: u64,
}

pub struct CollectOptions {
    pub participant: String,
    pub operation: String,
    pub experience: String,
    pub consent: bool,
    pub document: Option<PathBuf>,
    pub reference: Option<PathBuf>,
    pub password_file: Option<PathBuf>,
    pub include_document_digest: bool,
}

pub fn pseudonym(value: &str) -> Result<()> {
    require(
        value.len() == 8
            && value.starts_with("beta-")
            && value.as_bytes()[5..].iter().all(u8::is_ascii_digit),
        "INVALID_PARTICIPANT",
        "Use an anonymous participant identifier such as beta-001",
    )
}

pub fn known_diagnostics(root: &Path) -> Result<BTreeSet<String>> {
    let mut codes = BTreeSet::new();
    for file in ["PRODUCT_SCOPE.md", "AGENT_PROTOCOL.md"] {
        let bytes = read_bytes(&contained_file(root, file)?, MAX_JSON_BYTES)?;
        for word in text(&bytes)?
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        {
            if is_code(word) {
                codes.insert(word.to_owned());
            }
        }
    }
    Ok(codes)
}

pub fn validate_report(report: Report, allowed: &BTreeSet<String>) -> Result<Report> {
    require(
        report.schema == "docsight.beta-report/v1",
        "INVALID_BETA_SCHEMA",
        "Unsupported beta report schema",
    )?;
    checked_version(&report.version)?;
    checked_revision(&report.revision)?;
    checked_target(&report.target)?;
    pseudonym(&report.participant)?;
    checked_digest(&report.archive_sha256)?;
    if let Some(hash) = &report.document_sha256 {
        checked_digest(hash)?;
    }
    require(
        OPERATIONS.contains(&report.operation.as_str())
            && EXPERIENCES.contains(&report.experience.as_str())
            && OUTCOMES.contains(&report.outcome.as_str()),
        "INVALID_BETA_VALUE",
        "Unsupported beta operation, experience or outcome",
    )?;
    require(
        (i64::from(i32::MIN)..=i64::from(u32::MAX)).contains(&report.exit_code)
            && report.elapsed_ms <= 3_600_000
            && [
                report.stdout_bytes,
                report.stderr_bytes,
                report.document_size_bytes,
            ]
            .iter()
            .all(|value| *value <= i64::MAX as u64)
            && report.unknown_diagnostic_count <= 100_000,
        "INVALID_INTEGER",
        "Beta metadata exceeds its numeric limits",
    )?;
    require(
        report.diagnostic_codes.len() <= 512
            && report
                .diagnostic_codes
                .iter()
                .all(|code| allowed.contains(code))
            && report
                .diagnostic_codes
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
        "INVALID_DIAGNOSTIC_CODES",
        "Only sorted unique documented diagnostic codes may be shared",
    )?;
    let abnormal = !(0..=65535).contains(&report.exit_code);
    let consistent = match report.outcome.as_str() {
        "success" => report.exit_code == 0,
        "error" => (1..=65535).contains(&report.exit_code),
        "crash" => abnormal,
        "invalid_protocol" => !abnormal,
        "timeout" | "output_limit" => true,
        _ => false,
    };
    require(
        consistent,
        "INCONSISTENT_BETA_OUTCOME",
        "Beta outcome conflicts with its process exit code",
    )?;
    Ok(report)
}

fn project_diagnostics(
    result: &ProcessResult,
    allowed: &BTreeSet<String>,
) -> (Vec<String>, u64, bool) {
    let source = if result.returncode == 0 {
        &result.stdout
    } else {
        &result.stderr
    };
    let Ok(value) = parse_json(source) else {
        return (Vec::new(), 0, false);
    };
    let Some(object) = value.as_object() else {
        return (Vec::new(), 0, false);
    };
    let mut records: Vec<&Value> = object
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    if let Some(error) = object.get("error").filter(|error| error.is_object()) {
        records.push(error);
    }
    if object.get("code").is_some_and(Value::is_string) {
        records.push(&value);
    }
    let mut codes = BTreeSet::new();
    let mut unknown = 0;
    for record in records {
        match record.get("code").and_then(Value::as_str) {
            Some(code) if allowed.contains(code) => {
                codes.insert(code.to_owned());
            }
            _ => unknown += 1,
        }
    }
    let canonical = object.get("schema").and_then(Value::as_str) == Some("docsight.agent/v2")
        && if result.returncode == 0 {
            object.get("result").is_some_and(Value::is_object) && result.stderr.is_empty()
        } else {
            value.pointer("/error/exit_code").and_then(Value::as_i64) == Some(result.returncode)
                && result.stdout.is_empty()
        };
    (codes.into_iter().collect(), unknown, canonical)
}

pub fn summarize(
    result: &ProcessResult,
    manifest: &crate::release::archive::Manifest,
    archive_hash: &str,
    options: &CollectOptions,
    document_size: u64,
    document_hash: Option<String>,
    allowed: &BTreeSet<String>,
) -> Result<Report> {
    let (diagnostic_codes, unknown_diagnostic_count, protocol) =
        project_diagnostics(result, allowed);
    let outcome = match result.termination {
        Some(Termination::Timeout) => "timeout",
        Some(Termination::OutputLimit) => "output_limit",
        None if !(0..=65535).contains(&result.returncode) => "crash",
        None if !protocol => "invalid_protocol",
        None if result.returncode == 0 => "success",
        None => "error",
    };
    let report = Report {
        schema: "docsight.beta-report/v1".into(),
        version: manifest.version.clone(),
        revision: manifest.revision.clone(),
        target: manifest.target.clone(),
        archive_sha256: archive_hash.into(),
        participant: options.participant.clone(),
        operation: options.operation.clone(),
        experience: options.experience.clone(),
        outcome: outcome.into(),
        exit_code: result.returncode,
        elapsed_ms: result.elapsed_ms,
        stdout_bytes: u64::try_from(result.stdout.len())
            .map_err(|_| ToolError::new("INVALID_INTEGER", "Output size overflow"))?,
        stderr_bytes: u64::try_from(result.stderr.len())
            .map_err(|_| ToolError::new("INVALID_INTEGER", "Output size overflow"))?,
        document_size_bytes: document_size,
        document_sha256: document_hash,
        diagnostic_codes,
        unknown_diagnostic_count,
    };
    validate_report(report, allowed)
}

fn validate_options(options: &CollectOptions) -> Result<()> {
    require(
        options.consent,
        "BETA_CONSENT_REQUIRED",
        "Explicit consent is required before collecting an observation",
    )?;
    pseudonym(&options.participant)?;
    require(
        OPERATIONS.contains(&options.operation.as_str())
            && EXPERIENCES.contains(&options.experience.as_str()),
        "INVALID_BETA_OPERATION",
        "Choose a documented beta operation and experience",
    )?;
    require(
        options.operation == "capabilities" || options.document.is_some(),
        "BETA_DOCUMENT_REQUIRED",
        "This operation requires an explicitly selected document",
    )?;
    require(
        (options.operation == "diff") == options.reference.is_some(),
        "BETA_REFERENCE_REQUIRED",
        "Only diff requires a reference document",
    )?;
    require(
        options.operation != "capabilities"
            || (options.document.is_none()
                && options.password_file.is_none()
                && !options.include_document_digest),
        "UNUSED_BETA_INPUT",
        "Capabilities does not consume document or password inputs",
    )
}

pub fn collect_with<R: Runner>(
    archive: &Path,
    options: &CollectOptions,
    runner: &mut R,
) -> Result<Report> {
    validate_options(options)?;
    let manifest = verify_archive(archive)?;
    require(
        manifest.target == native_target()?,
        "BETA_HOST_MISMATCH",
        "Beta collection requires a native archive",
    )?;
    let archive_hash = sha256_file(archive)?;
    let mut inputs = Vec::new();
    for path in [&options.document, &options.reference]
        .into_iter()
        .flatten()
    {
        let bytes = read_bytes(path, MAX_FILE_BYTES)?;
        inputs.push((
            path.clone(),
            digest(&bytes),
            u64::try_from(bytes.len())
                .map_err(|_| ToolError::new("INVALID_INTEGER", "Document size overflow"))?,
        ));
    }
    if let Some(password) = &options.password_file {
        read_bytes(password, 131)?;
    }
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let (binary, manifest) = extract_verified(archive, &root.join("package"))?;
    let work = root.join("work");
    fs::create_dir(&work)?;
    let environment = isolated_environment()?;
    let version = runner.run(
        &[binary.as_os_str().to_owned(), "--version".into()],
        &work,
        &ProcessLimits {
            timeout: Duration::from_secs(10),
            output_bytes: 4096,
        },
        Some(&environment),
    )?;
    require(
        version.termination.is_none()
            && version.returncode == 0
            && version.stderr.is_empty()
            && text(&version.stdout)?
                .trim()
                .starts_with(&format!("docsight {}", manifest.version)),
        "BETA_VERSION_MISMATCH",
        "Executable version differs from archive manifest",
    )?;
    let mut args: Vec<OsString> = vec![
        binary.into_os_string(),
        "--agent".into(),
        "--sandbox".into(),
        "--max-bytes".into(),
        "65536".into(),
    ];
    if let Some(password) = &options.password_file {
        args.extend([
            "--password-file".into(),
            password.canonicalize()?.into_os_string(),
        ]);
    }
    args.push(options.operation.clone().into());
    for path in [&options.document, &options.reference]
        .into_iter()
        .flatten()
    {
        args.push(path.canonicalize()?.into_os_string());
    }
    if options.operation == "render" {
        args.extend([
            "--page".into(),
            "1".into(),
            "--dpi".into(),
            "72".into(),
            "--out".into(),
            work.join("render.png").into_os_string(),
        ]);
    }
    let result = runner.run(
        &args,
        &work,
        &ProcessLimits {
            timeout: Duration::from_secs(45),
            output_bytes: 262_144,
        },
        Some(&environment),
    )?;
    for (path, hash, _) in &inputs {
        require(
            sha256_file(path)? == *hash,
            "BETA_INPUT_CHANGED",
            "An input changed during collection",
        )?;
    }
    require(
        sha256_file(archive)? == archive_hash,
        "ARCHIVE_CHANGED",
        "Archive changed during collection",
    )?;
    let size = inputs.first().map(|entry| entry.2).unwrap_or(0);
    let document_hash = if options.include_document_digest {
        inputs.first().map(|entry| entry.1.clone())
    } else {
        None
    };
    summarize(
        &result,
        &manifest,
        &archive_hash,
        options,
        size,
        document_hash,
        &known_diagnostics(&root.join("package"))?,
    )
}

pub fn collect(archive: &Path, options: &CollectOptions) -> Result<Report> {
    collect_with(archive, options, &mut NativeRunner)
}

fn count<'a>(values: impl Iterator<Item = &'a str>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value.to_owned()).or_insert(0) += 1;
    }
    counts
}

pub fn aggregate(directory: &Path, root: &Path) -> Result<Value> {
    let paths = flat_files(directory, "json", 10_000)?;
    require(
        !paths.is_empty(),
        "NO_BETA_REPORTS",
        "No beta observations were collected",
    )?;
    let allowed = known_diagnostics(root)?;
    let mut reports = Vec::new();
    let mut canonical = BTreeSet::new();
    let mut hashes = Vec::new();
    let mut identities = BTreeSet::new();
    for path in paths {
        let bytes = read_bytes(&path, 262_144)?;
        let report = validate_report(decode(parse_json(&bytes)?)?, &allowed)?;
        require(
            canonical.insert(digest(&json_bytes(&report)?)),
            "DUPLICATE_BETA_REPORT",
            "Copied reports cannot count as additional observations",
        )?;
        hashes.push(digest(&bytes));
        identities.insert((report.version.clone(), report.revision.clone()));
        reports.push(report);
    }
    require(
        identities.len() == 1,
        "MIXED_BETA_CANDIDATES",
        "Reports must describe the same candidate",
    )?;
    let (version, revision) = identities
        .into_iter()
        .next()
        .ok_or_else(|| ToolError::new("NO_BETA_REPORTS", "No observations"))?;
    let mut performance = BTreeMap::new();
    for operation in OPERATIONS {
        let mut samples: Vec<u64> = reports
            .iter()
            .filter(|report| report.operation == operation)
            .map(|report| report.elapsed_ms)
            .collect();
        samples.sort_unstable();
        let n = samples.len();
        if n > 0 {
            let p50 = if n.is_multiple_of(2) {
                json!((samples[n / 2 - 1] + samples[n / 2]) as f64 / 2.0)
            } else {
                json!(samples[n / 2])
            };
            performance.insert(
                operation,
                json!({
                    "samples": n,
                    "p50_ms": p50,
                    "p95_ms": samples[(95 * n).div_ceil(100) - 1],
                    "maximum_ms": samples[n - 1],
                }),
            );
        }
    }
    hashes.sort();
    let participants: BTreeSet<_> = reports.iter().map(|report| &report.participant).collect();
    let archives: BTreeSet<_> = reports
        .iter()
        .map(|report| &report.archive_sha256)
        .collect();
    Ok(json!({
        "schema": "docsight.beta-summary/v1",
        "version": version,
        "revision": revision,
        "participants": participants,
        "reports": reports.len(),
        "report_sha256s": hashes,
        "archive_sha256s": archives,
        "operations": count(reports.iter().map(|report| report.operation.as_str())),
        "outcomes": count(reports.iter().map(|report| report.outcome.as_str())),
        "experience": count(reports.iter().map(|report| report.experience.as_str())),
        "diagnostics": count(
            reports
                .iter()
                .flat_map(|report| report.diagnostic_codes.iter().map(String::as_str))
        ),
        "performance": performance,
    }))
}
