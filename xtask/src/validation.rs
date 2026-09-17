use crate::tooling::{common::*, process::{NativeRunner, ProcessLimits, Runner, Termination}};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub const CHECK_NAMES: [&str; 9] = ["rust_tooling_tests", "rust_only", "corpus_inventory", "cargo_fmt", "cargo_clippy", "cargo_test", "cargo_build", "ds9_benchmark", "git_diff_check"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status { Pass, Fail, Unavailable, Blocked, NotRun }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check { pub name: String, pub status: Status, #[serde(deserialize_with = "required_option")] pub reason: Option<String>, #[serde(deserialize_with = "required_option")] pub exit_code: Option<i64>, pub elapsed_ms: u64, pub stdout_log: String, pub stderr_log: String, pub stdout_sha256: String, pub stderr_sha256: String, }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report { pub schema: String, pub version: String, pub revision: String, pub clean_tree_before: bool, pub clean_tree_after: bool, pub same_revision_after: bool, pub checks: Vec<Check>, pub passed: bool, }

pub fn commands(output: &Path, executable: &Path) -> Vec<Vec<OsString>> {
    let raw: [&[&str]; 9] = [
        &["cargo", "test", "--locked", "-p", "xtask", "--all-targets"], &["", "rust-only"], &["", "corpus", "validate"],
        &["cargo", "fmt", "--all", "--check"], &["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        &["cargo", "test", "--locked", "--workspace", "--all-features"], &["cargo", "build", "--locked", "--release", "--workspace", "--all-features"],
        &["cargo", "run", "--locked", "--release", "-p", "xtask", "--", "benchmark", "--check", "--output"], &["git", "diff", "--check"],
    ];
    let mut commands: Vec<Vec<OsString>> = raw.iter().map(|args| args.iter().map(|arg| OsString::from(*arg)).collect()).collect();
    commands[1][0] = executable.as_os_str().to_owned(); commands[2][0] = executable.as_os_str().to_owned(); commands[7].push(output.join("ds9-performance.json").into_os_string()); commands
}

pub fn git_state<R: Runner>(root: &Path, runner: &mut R) -> Result<(String, bool)> {
    let limits = ProcessLimits { timeout: Duration::from_secs(10), output_bytes: 1_048_576 };
    let revision = runner.run(&["git".into(), "rev-parse".into(), "HEAD".into()], root, &limits, None)?;
    let status = runner.run(&["git".into(), "status".into(), "--porcelain".into(), "--untracked-files=all".into()], root, &limits, None)?;
    require(revision.returncode == 0 && status.returncode == 0 && revision.termination.is_none() && status.termination.is_none(), "GIT_STATE_UNAVAILABLE", "Validation requires a readable Git revision and working tree")?;
    let sha = text(&revision.stdout)?.trim().to_owned(); checked_revision(&sha)?; Ok((sha, status.stdout.is_empty()))
}

pub fn run_with<R: Runner>(output: &Path, root: &Path, executable: &Path, runner: &mut R) -> Result<Report> {
    let (revision, clean_tree_before) = git_state(root, runner)?;
    if let Some(parent) = output.parent().filter(|parent| !parent.as_os_str().is_empty()) { fs::create_dir_all(parent)?; }
    fs::create_dir(output)?;
    let mut checks = Vec::new(); let mut selected = commands(output, executable); for index in [1, 2] { selected[index].extend(["--root".into(), root.as_os_str().to_owned()]); }
    for (name, arguments) in CHECK_NAMES.into_iter().zip(selected) {
        let result = runner.run(&arguments, root, &ProcessLimits { timeout: Duration::from_secs(3600), output_bytes: 16_777_216 }, None);
        let (status, reason, exit_code, elapsed_ms, stdout, stderr) = match result {
            Ok(result) => { let reason = result.termination.map(|termination| match termination { Termination::Timeout => "timeout".to_owned(), Termination::OutputLimit => "output_limit".to_owned(), }); let status = if result.returncode == 0 && reason.is_none() { Status::Pass } else { Status::Fail }; (status, reason, Some(result.returncode), result.elapsed_ms, result.stdout, result.stderr) }
            Err(error) => { let state = match error.code { "EXECUTABLE_UNAVAILABLE" => Status::Unavailable, "UNSUPPORTED_HOST" => Status::NotRun, _ => Status::Blocked }; (state, Some(error.code.to_owned()), None, 0, Vec::new(), json_bytes(&error)?) }
        };
        let stdout_log = format!("{name}.stdout.log"); let stderr_log = format!("{name}.stderr.log");
        write_new(&output.join(&stdout_log), &stdout, false)?; write_new(&output.join(&stderr_log), &stderr, false)?;
        checks.push(Check { name: name.into(), status, reason, exit_code, elapsed_ms, stdout_log, stderr_log, stdout_sha256: digest(&stdout), stderr_sha256: digest(&stderr) });
    }
    let (after, clean_tree_after) = git_state(root, runner)?; let same_revision_after = revision == after;
    let report = Report { schema: "docsight.validation/v2".into(), version: workspace_version().into(), revision, clean_tree_before, clean_tree_after, same_revision_after, passed: clean_tree_before && clean_tree_after && same_revision_after && checks.iter().all(|check| check.status == Status::Pass), checks };
    write_new(&output.join("validation.json"), &json_bytes(&report)?, false)?; Ok(report)
}

pub fn run(output: &Path, root: &Path) -> Result<Report> { run_with(output, root, &std::env::current_exe()?, &mut NativeRunner) }
