#[allow(dead_code)]
mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::tooling::common::{self, parse_json, read_json, sha256_file};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Termination};
use xtask::validation::{CHECK_NAMES, Report, Status, commands, run_with};

type Arguments = [OsString];

fn words(arguments: &Arguments) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

struct Workspace {
    fixture: Fixture,
    executable: PathBuf,
}

impl Workspace {
    fn new() -> TestResult<Self> {
        let fixture = Fixture::new()?;
        let executable = fixture.root.join("maint");
        Ok(Self {
            fixture,
            executable,
        })
    }

    fn output(&self) -> PathBuf {
        self.fixture.root.join("evidence/validation")
    }

    fn run<F>(&self, mut gate: F) -> common::Result<Report>
    where
        F: FnMut(&[String]) -> common::Result<ProcessResult>,
    {
        let mut runner = Callback(
            |arguments: &Arguments,
             _: &Path,
             _: &ProcessLimits,
             _: Option<&BTreeMap<OsString, OsString>>| {
                let words = words(arguments);
                match words
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["git", "rev-parse", "HEAD"] => {
                        Ok(outcome(0, format!("{REVISION}\n").into_bytes(), Vec::new()))
                    }
                    ["git", "status", ..] => Ok(outcome(0, Vec::new(), Vec::new())),
                    _ => gate(&words),
                }
            },
        );
        run_with(
            &self.output(),
            &self.fixture.root,
            &self.executable,
            &mut runner,
        )
    }
}

fn passing(_: &[String]) -> common::Result<ProcessResult> {
    Ok(outcome(0, b"gate output".to_vec(), Vec::new()))
}

#[test]
fn every_gate_invokes_its_canonical_command() -> TestResult {
    let output = PathBuf::from("evidence");
    let executable = PathBuf::from("maint");
    let selected: Vec<Vec<String>> = commands(&output, &executable)
        .iter()
        .map(|arguments| words(arguments))
        .collect();
    assert_eq!(selected.len(), CHECK_NAMES.len());
    assert_eq!(selected[1], ["maint", "rust-only"]);
    assert_eq!(selected[2], ["maint", "corpus", "validate"]);
    assert_eq!(
        selected[7],
        [
            "cargo",
            "run",
            "--locked",
            "--release",
            "-p",
            "xtask",
            "--bin",
            "xtask",
            "--",
            "benchmark",
            "--check",
            "--output",
            &output.join("ds9-performance.json").to_string_lossy(),
        ]
    );
    assert!(Path::new(env!("CARGO_BIN_EXE_xtask")).is_file());
    assert!(
        selected
            .iter()
            .filter(|command| command.first().is_some_and(|program| program == "cargo"))
            .filter(|command| command[1] != "fmt")
            .all(|command| command.contains(&"--locked".to_owned()))
    );
    Ok(())
}

#[test]
fn passing_gates_are_recorded_with_bound_logs() -> TestResult {
    let workspace = Workspace::new()?;
    let mut invoked = Vec::new();
    let report = workspace.run(|words| {
        invoked.push(words.to_vec());
        passing(words)
    })?;
    assert!(report.passed);
    assert!(report.clean_tree_before && report.clean_tree_after && report.same_revision_after);
    assert_eq!(report.revision, REVISION);
    let names: Vec<_> = report
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    assert_eq!(names, CHECK_NAMES);
    let root = workspace.fixture.root.to_string_lossy().into_owned();
    for index in [1, 2] {
        assert_eq!(
            invoked[index][invoked[index].len() - 2..],
            ["--root".to_owned(), root.clone()]
        );
    }
    for check in &report.checks {
        assert_eq!(check.status, Status::Pass);
        assert_eq!(check.exit_code, Some(0));
        let stdout = workspace.output().join(&check.stdout_log);
        let stderr = workspace.output().join(&check.stderr_log);
        assert_eq!(sha256_file(&stdout)?, check.stdout_sha256);
        assert_eq!(sha256_file(&stderr)?, check.stderr_sha256);
        assert_eq!(fs::read(stdout)?, b"gate output");
    }
    let stored = read_json(&workspace.output().join("validation.json"))?;
    assert_eq!(stored["schema"], "docsight.validation/v2");
    assert_eq!(stored["passed"], true);
    Ok(())
}

#[test]
fn unavailable_blocked_and_failed_gates_never_look_like_passes() -> TestResult {
    let workspace = Workspace::new()?;
    let report = workspace.run(|words| match words.get(1).map(String::as_str) {
        Some("fmt") => Err(common::ToolError::new(
            "EXECUTABLE_UNAVAILABLE",
            "Required executable is unavailable",
        )),
        Some("clippy") => Err(common::ToolError::new(
            "PROCESS_GROUP",
            "Cannot attach process",
        )),
        Some("build") => Err(common::ToolError::new(
            "UNSUPPORTED_HOST",
            "Process groups require Unix or Windows",
        )),
        Some("test") if words.contains(&"--workspace".to_owned()) => {
            Ok(outcome(101, Vec::new(), b"test failed".to_vec()))
        }
        Some("run") => {
            let mut result = outcome(0, Vec::new(), Vec::new());
            result.termination = Some(Termination::Timeout);
            Ok(result)
        }
        _ => passing(words),
    })?;
    assert!(!report.passed);
    assert_eq!(report.checks.len(), CHECK_NAMES.len());
    let by_name: BTreeMap<_, _> = report
        .checks
        .iter()
        .map(|check| (check.name.as_str(), check))
        .collect();
    let formatter = by_name["cargo_fmt"];
    assert_eq!(formatter.status, Status::Unavailable);
    assert_eq!(formatter.exit_code, None);
    assert_eq!(formatter.reason.as_deref(), Some("EXECUTABLE_UNAVAILABLE"));
    let logged = parse_json(&fs::read(workspace.output().join(&formatter.stderr_log))?)?;
    assert_eq!(logged["code"], "EXECUTABLE_UNAVAILABLE");
    assert_eq!(by_name["cargo_clippy"].status, Status::Blocked);
    assert_eq!(by_name["cargo_build"].status, Status::NotRun);
    assert_eq!(by_name["cargo_test"].status, Status::Fail);
    assert_eq!(by_name["cargo_test"].exit_code, Some(101));
    assert_eq!(by_name["ds9_benchmark"].status, Status::Fail);
    assert_eq!(by_name["ds9_benchmark"].reason.as_deref(), Some("timeout"));
    assert_eq!(by_name["git_diff_check"].status, Status::Pass);
    Ok(())
}

#[test]
fn dirty_or_moving_revisions_fail_even_when_every_gate_passes() -> TestResult {
    let workspace = Workspace::new()?;
    let mut status_calls = 0;
    let mut runner = Callback(
        |arguments: &Arguments,
         _: &Path,
         _: &ProcessLimits,
         _: Option<&BTreeMap<OsString, OsString>>| {
            let words = words(arguments);
            if words.starts_with(&["git".to_owned(), "status".to_owned()]) {
                status_calls += 1;
                return Ok(outcome(0, b"?? untracked.txt\n".to_vec(), Vec::new()));
            }
            if words.starts_with(&["git".to_owned(), "rev-parse".to_owned()]) {
                return Ok(outcome(0, format!("{REVISION}\n").into_bytes(), Vec::new()));
            }
            passing(&words)
        },
    );
    let report = run_with(
        &workspace.output(),
        &workspace.fixture.root,
        &workspace.executable,
        &mut runner,
    )?;
    assert_eq!(status_calls, 2);
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status == Status::Pass)
    );
    assert!(!report.clean_tree_before);
    assert!(!report.passed);

    let workspace = Workspace::new()?;
    let mut revisions = 0;
    let mut runner = Callback(
        |arguments: &Arguments,
         _: &Path,
         _: &ProcessLimits,
         _: Option<&BTreeMap<OsString, OsString>>| {
            let words = words(arguments);
            if words.starts_with(&["git".to_owned(), "rev-parse".to_owned()]) {
                revisions += 1;
                let revision = if revisions == 1 {
                    REVISION.to_owned()
                } else {
                    "b".repeat(40)
                };
                return Ok(outcome(0, format!("{revision}\n").into_bytes(), Vec::new()));
            }
            if words.starts_with(&["git".to_owned(), "status".to_owned()]) {
                return Ok(outcome(0, Vec::new(), Vec::new()));
            }
            passing(&words)
        },
    );
    let report = run_with(
        &workspace.output(),
        &workspace.fixture.root,
        &workspace.executable,
        &mut runner,
    )?;
    assert!(!report.same_revision_after);
    assert!(!report.passed);
    Ok(())
}

#[test]
fn existing_evidence_and_unreadable_git_state_stop_before_any_gate() -> TestResult {
    let workspace = Workspace::new()?;
    fs::create_dir_all(workspace.output())?;
    let mut gates = 0;
    let result = workspace.run(|words| {
        gates += 1;
        passing(words)
    });
    assert!(result.is_err());
    assert_eq!(gates, 0);

    let workspace = Workspace::new()?;
    let mut runner = Callback(
        |_: &Arguments, _: &Path, _: &ProcessLimits, _: Option<&BTreeMap<OsString, OsString>>| {
            Ok(outcome(128, Vec::new(), b"not a git repository".to_vec()))
        },
    );
    assert_eq!(
        code(run_with(
            &workspace.output(),
            &workspace.fixture.root,
            &workspace.executable,
            &mut runner
        )),
        Some("GIT_STATE_UNAVAILABLE")
    );
    assert!(!workspace.output().exists());
    Ok(())
}
