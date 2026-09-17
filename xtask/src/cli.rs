use crate::{architecture, beta, corpus, readiness, release, smoke, validation};
use crate::tooling::common::*;
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "xtask", version, about = "Native DocSight maintenance and evidence tooling", after_help = "The existing benchmark command also accepts: benchmark --check --output PATH")]
pub struct Cli {
    #[arg(long, global = true)] root: Option<PathBuf>,
    #[command(subcommand)] command: Command,
}

#[derive(Subcommand)]
enum Command {
    Release { #[command(subcommand)] command: ReleaseCommand },
    Notices { #[arg(long)] metadata: PathBuf, #[arg(long)] out: PathBuf },
    Changelog { #[arg(long)] revision: String, #[arg(long)] since: Option<String>, #[arg(long)] out: PathBuf },
    Smoke { archive: PathBuf, #[arg(long)] out: PathBuf },
    Corpus { #[command(subcommand)] command: CorpusCommand },
    Beta { #[command(subcommand)] command: BetaCommand },
    Validate { #[arg(long)] out: PathBuf },
    Readiness { #[arg(long)] evidence: PathBuf, #[arg(long)] revision: String, #[arg(long)] out: Option<PathBuf> },
    RustOnly,
}

#[derive(Subcommand)]
enum ReleaseCommand {
    Matrix,
    Configuration { #[arg(long)] github_output: Option<PathBuf> },
    Version { #[arg(long)] tag: Option<String> },
    Package { #[arg(long)] binary: PathBuf, #[arg(long)] worker: PathBuf, #[arg(long)] target: String, #[arg(long)] revision: String, #[arg(long)] notices: PathBuf, #[arg(long)] out: PathBuf },
    Verify { archive: PathBuf },
    Collect { directory: PathBuf },
}

#[derive(Subcommand)]
enum CorpusCommand {
    Validate { #[arg(long)] manifest: Option<PathBuf> },
    Run { #[arg(long)] archive: PathBuf, #[arg(long)] manifest: Option<PathBuf>, #[arg(long)] out: PathBuf },
}

#[derive(Args)]
struct Collection {
    #[arg(long)] archive: PathBuf,
    #[arg(long)] participant: String,
    #[arg(long)] operation: String,
    #[arg(long)] experience: String,
    #[arg(long, required = true)] consent: bool,
    #[arg(long)] document: Option<PathBuf>,
    #[arg(long)] reference: Option<PathBuf>,
    #[arg(long)] password_file: Option<PathBuf>,
    #[arg(long)] include_document_digest: bool,
    #[arg(long)] out: PathBuf,
}

#[derive(Subcommand)]
enum BetaCommand { Collect(Collection), Aggregate { directory: PathBuf, #[arg(long)] out: Option<PathBuf> }, }

fn emit<T: Serialize>(value: &T, output: Option<&Path>) -> Result<()> {
    let bytes = json_bytes(value)?;
    if let Some(path) = output {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) { fs::create_dir_all(parent)?; no_symlinks(parent)?; }
        write_new(path, &bytes, false)?;
    }
    io::stdout().lock().write_all(&bytes)?; Ok(())
}

fn emit_observation(report: &beta::Report, output: &Path) -> Result<bool> { emit(report, Some(output))?; Ok(true) }
fn output_text(path: &Path, content: &str) -> Result<()> { if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) { fs::create_dir_all(parent)?; no_symlinks(parent)?; } write_new(path, content.as_bytes(), false) }

pub fn execute(cli: Cli) -> Result<bool> {
    let root = fs::canonicalize(cli.root.unwrap_or_else(workspace_root))?;
    match cli.command {
        Command::Release { command } => match command {
            ReleaseCommand::Matrix => emit(&release::matrix(&root)?, None)?,
            ReleaseCommand::Configuration { github_output } => {
                let matrix = release::matrix(&root)?; let version = workspace_version();
                if let Some(path) = github_output.or_else(|| std::env::var_os("GITHUB_OUTPUT").map(PathBuf::from)) {
                    no_symlinks(&path)?; let metadata = fs::metadata(&path)?; require(metadata.is_file(), "INVALID_OUTPUT", "GitHub output must be a regular file")?;
                    let mut output = OpenOptions::new().append(true).open(path)?; writeln!(output, "matrix={}", serde_json::to_string(&matrix)?)?; writeln!(output, "version={version}")?; output.sync_all()?;
                }
                emit(&json!({"matrix": matrix, "version": version}), None)?;
            }
            ReleaseCommand::Version { tag } => { if let Some(tag) = tag { require(tag == format!("v{}", workspace_version()), "TAG_VERSION_MISMATCH", "Selected tag must match the workspace version")?; } emit(&json!({"version": workspace_version()}), None)?; }
            ReleaseCommand::Package { binary, worker, target, revision, notices, out } => { let archive = release::archive::make_package(&root, &binary, &worker, &target, &revision, &notices, &out)?; emit(&json!({"archive": archive.file_name().and_then(|name| name.to_str()), "sha256": sha256_file(&archive)?}), None)?; }
            ReleaseCommand::Verify { archive } => emit(&release::archive::verify_archive(&archive)?, None)?,
            ReleaseCommand::Collect { directory } => emit(&release::archive::collect(&directory)?, None)?,
        },
        Command::Notices { metadata, out } => { let content = release::notices::generate(read_json_limit(&metadata, 16_777_216)?, &root.join("Cargo.lock"))?; output_text(&out, &content)?; emit(&json!({"schema": "docsight.notices-result/v1", "sha256": digest(content.as_bytes())}), None)?; }
        Command::Changelog { revision, since, out } => { let content = release::changelog::generate(&root, workspace_version(), &revision, since.as_deref().filter(|value| !value.is_empty()))?; output_text(&out, &content)?; emit(&json!({"schema": "docsight.changelog-result/v1", "revision": revision, "sha256": digest(content.as_bytes())}), None)?; }
        Command::Smoke { archive, out } => { let receipt = smoke::smoke_archive(&archive)?; emit(&receipt, Some(&out))?; return Ok(receipt.passed); }
        Command::Corpus { command } => match command {
            CorpusCommand::Validate { manifest } => { let path = manifest.unwrap_or_else(|| root.join("release/corpus.json")); let manifest = corpus::load_manifest(&path, Some(&root))?; emit(&json!({"schema": "docsight.corpus-inventory/v1", "manifest_sha256": sha256_file(&path)?, "cases": manifest.cases.len(), "executed": false}), None)?; }
            CorpusCommand::Run { archive, manifest, out } => { let path = manifest.unwrap_or_else(|| root.join("release/corpus.json")); let report = corpus::run_corpus(&archive, &path, &root)?; emit(&report, Some(&out))?; return Ok(report.passed); }
        },
        Command::Beta { command } => match command {
            BetaCommand::Collect(options) => { let settings = beta::CollectOptions { participant: options.participant, operation: options.operation, experience: options.experience, consent: options.consent, document: options.document, reference: options.reference, password_file: options.password_file, include_document_digest: options.include_document_digest }; let report = beta::collect(&options.archive, &settings)?; return emit_observation(&report, &options.out); }
            BetaCommand::Aggregate { directory, out } => emit(&beta::aggregate(&directory, &root)?, out.as_deref())?,
        },
        Command::Validate { out } => { let report = validation::run(&out, &root)?; emit(&report, None)?; return Ok(report.passed); }
        Command::Readiness { evidence, revision, out } => { let report = readiness::assess(&evidence, &revision, &root)?; emit(&report, out.as_deref())?; return Ok(report.ready_for_v1); }
        Command::RustOnly => { let report = architecture::audit(&root)?; emit(&report, None)?; return Ok(report.get("rust_only").and_then(serde_json::Value::as_bool) == Some(true)); }
    }
    Ok(true)
}

pub fn entry() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => { match serde_json::to_string(&error) { Ok(message) => eprintln!("{message}"), Err(_) => eprintln!("{{\"schema\":\"docsight.tooling-error/v1\",\"code\":\"SERIALIZATION_ERROR\"}}"), } ExitCode::from(2) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collecting_a_failed_engine_outcome_still_writes_a_successful_observation() -> Result<()> {
        let temporary = tempfile::tempdir()?; let path = temporary.path().join("observation.json");
        let report = beta::Report { schema: "docsight.beta-report/v1".into(), version: workspace_version().into(), revision: "a".repeat(40), target: "x86_64-unknown-linux-gnu".into(), archive_sha256: "b".repeat(64), participant: "beta-001".into(), operation: "inspect".into(), experience: "blocked".into(), outcome: "error".into(), exit_code: 10, elapsed_ms: 1, stdout_bytes: 0, stderr_bytes: 100, document_size_bytes: 10, document_sha256: None, diagnostic_codes: vec!["UNSUPPORTED_FORMAT".into()], unknown_diagnostic_count: 0 };
        assert!(emit_observation(&report, &path)?); let stored = read_json(&path)?; assert_eq!(stored["outcome"], "error"); assert_eq!(stored["exit_code"], 10); assert!(emit_observation(&report, &path).is_err()); Ok(())
    }
}
