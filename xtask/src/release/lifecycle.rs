use super::archive::{Manifest, extract_verified, verify_archive, verify_sidecar};
use super::{Check, EXAMPLES, basename};
use crate::tooling::common::*;
use crate::tooling::process::{ProcessLimits, ProcessResult, Runner, isolated_environment};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const LIFECYCLE_SCHEMA: &str = "docsight.release-lifecycle/v1";
pub const LIFECYCLE_CHECKS: [&str; 10] = [
    "install_previous",
    "baseline_previous",
    "install_candidate_side_by_side",
    "previous_unchanged",
    "candidate_runs",
    "candidate_reads_previous_examples",
    "compatibility",
    "rollback",
    "remove_candidate",
    "no_external_state",
];
const STATE_DIRECTORIES: [&str; 6] = ["home", "config", "cache", "data", "temp", "appdata"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub version: String,
    pub revision: String,
    pub archive_sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub protocol_changed: bool,
    pub coordinate_system_changed: bool,
    pub removed_commands: Vec<String>,
    pub changed_error_codes: Vec<String>,
    pub removed_document_formats: Vec<String>,
    pub removed_schemas: Vec<String>,
    pub breaking: bool,
    pub breaking_change_declared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReceipt {
    pub schema: String,
    pub target: String,
    pub previous: Release,
    pub candidate: Release,
    pub checks: Vec<Check>,
    #[serde(deserialize_with = "required_option")]
    pub compatibility: Option<Compatibility>,
    pub passed: bool,
}

struct Installed {
    manifest: Manifest,
    directory: PathBuf,
    binary: PathBuf,
}

struct Lifecycle<'a, R: Runner> {
    runner: &'a mut R,
    environment: BTreeMap<OsString, OsString>,
    state: PathBuf,
    work: PathBuf,
    elapsed_ms: u64,
}

fn version_parts(version: &str) -> Result<(u64, u64, u64, bool)> {
    checked_version(version)?;
    let (core, prerelease) = match version.split_once('-') {
        Some((core, _)) => (core, true),
        None => (version, false),
    };
    let numbers: Vec<u64> = core
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| ToolError::new("INVALID_VERSION", "Version component is not a number"))
        })
        .collect::<Result<_>>()?;
    match numbers.as_slice() {
        [major, minor, patch] => Ok((*major, *minor, *patch, prerelease)),
        _ => Err(ToolError::new(
            "INVALID_VERSION",
            "Version must have three numeric components",
        )),
    }
}

pub fn is_update(previous: &str, candidate: &str) -> Result<bool> {
    let (major, minor, patch, previous_prerelease) = version_parts(previous)?;
    let (next_major, next_minor, next_patch, candidate_prerelease) = version_parts(candidate)?;
    let order = (next_major, next_minor, next_patch).cmp(&(major, minor, patch));
    Ok(order.is_gt() || (order.is_eq() && previous_prerelease && !candidate_prerelease))
}

pub fn declares_breaking_change(previous: &str, candidate: &str) -> Result<bool> {
    let (major, minor, _, _) = version_parts(previous)?;
    let (next_major, next_minor, _, _) = version_parts(candidate)?;
    Ok(next_major > major || (major == 0 && next_major == 0 && next_minor > minor))
}

fn names(value: &Value, pointer: &str, key: &str) -> Result<BTreeSet<String>> {
    let items = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolError::new(
                "LIFECYCLE_CAPABILITIES",
                "Capabilities omit a required list",
            )
        })?;
    items
        .iter()
        .map(|item| {
            let name = if key.is_empty() {
                item.as_str()
            } else {
                item.get(key).and_then(Value::as_str)
            };
            name.map(str::to_owned).ok_or_else(|| {
                ToolError::new(
                    "LIFECYCLE_CAPABILITIES",
                    "Capabilities list contains an unnamed entry",
                )
            })
        })
        .collect()
}

fn error_codes(value: &Value) -> Result<BTreeMap<String, i64>> {
    value
        .pointer("/errors/codes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ToolError::new(
                "LIFECYCLE_CAPABILITIES",
                "Capabilities omit the error code table",
            )
        })?
        .iter()
        .map(|entry| {
            match (
                entry.get("code").and_then(Value::as_str),
                entry.get("exit_code").and_then(Value::as_i64),
            ) {
                (Some(code), Some(exit)) => Ok((code.to_owned(), exit)),
                _ => Err(ToolError::new(
                    "LIFECYCLE_CAPABILITIES",
                    "Error code entry lacks its code or exit code",
                )),
            }
        })
        .collect()
}

fn schema_files(manifest: &Manifest) -> BTreeSet<String> {
    manifest
        .files
        .iter()
        .filter(|record| record.path.starts_with("schemas/"))
        .map(|record| record.path.clone())
        .collect()
}

pub fn compare_capabilities(
    previous: &Value,
    candidate: &Value,
    previous_manifest: &Manifest,
    candidate_manifest: &Manifest,
) -> Result<Compatibility> {
    let removed = |old: BTreeSet<String>, new: &BTreeSet<String>| -> Vec<String> {
        old.difference(new).cloned().collect()
    };
    let old_errors = error_codes(previous)?;
    let new_errors = error_codes(candidate)?;
    let mut compatibility = Compatibility {
        protocol_changed: previous.get("protocol") != candidate.get("protocol"),
        coordinate_system_changed: previous.get("coordinate_system")
            != candidate.get("coordinate_system"),
        removed_commands: removed(
            names(previous, "/commands", "name")?,
            &names(candidate, "/commands", "name")?,
        ),
        changed_error_codes: old_errors
            .iter()
            .filter(|(code, exit)| new_errors.get(*code) != Some(*exit))
            .map(|(code, _)| code.clone())
            .collect(),
        removed_document_formats: removed(
            names(previous, "/document_formats", "")?,
            &names(candidate, "/document_formats", "")?,
        ),
        removed_schemas: removed(
            schema_files(previous_manifest),
            &schema_files(candidate_manifest),
        ),
        breaking: false,
        breaking_change_declared: declares_breaking_change(
            &previous_manifest.version,
            &candidate_manifest.version,
        )?,
    };
    compatibility.breaking = compatibility.protocol_changed
        || compatibility.coordinate_system_changed
        || !compatibility.removed_commands.is_empty()
        || !compatibility.changed_error_codes.is_empty()
        || !compatibility.removed_document_formats.is_empty()
        || !compatibility.removed_schemas.is_empty();
    Ok(compatibility)
}

fn agent_result(output: &ProcessResult) -> Result<Value> {
    require(
        output.termination.is_none() && output.returncode == 0 && output.stderr.is_empty(),
        "LIFECYCLE_COMMAND_FAILED",
        "Installed command failed, wrote diagnostics or exceeded its budget",
    )?;
    let value = parse_json(&output.stdout)?;
    require(
        value.get("schema").and_then(Value::as_str) == Some("docsight.agent/v2"),
        "LIFECYCLE_PROTOCOL",
        "Installed command did not answer with the agent envelope",
    )?;
    Ok(field(&value, "result")?.clone())
}

impl<R: Runner> Lifecycle<'_, R> {
    fn run(&mut self, binary: &Path, arguments: &[OsString]) -> Result<ProcessResult> {
        let mut command = vec![binary.as_os_str().to_owned()];
        command.extend_from_slice(arguments);
        let result = self.runner.run(
            &command,
            &self.work,
            &ProcessLimits {
                timeout: Duration::from_secs(60),
                output_bytes: 8_388_608,
            },
            Some(&self.environment),
        )?;
        self.elapsed_ms = self.elapsed_ms.saturating_add(result.elapsed_ms);
        Ok(result)
    }

    fn version(&mut self, installed: &Installed) -> Result<()> {
        let output = self.run(&installed.binary, &["--version".into()])?;
        require(
            output.termination.is_none()
                && output.returncode == 0
                && output.stderr.is_empty()
                && text(&output.stdout)?
                    .trim()
                    .starts_with(&format!("docsight {}", installed.manifest.version)),
            "LIFECYCLE_VERSION_MISMATCH",
            "Installed executable does not report the version of its package",
        )
    }

    fn capabilities(&mut self, installed: &Installed) -> Result<Value> {
        let output = self.run(
            &installed.binary,
            &["--agent".into(), "capabilities".into()],
        )?;
        agent_result(&output)
    }

    fn inspect_examples(&mut self, installed: &Installed, examples: &Path) -> Result<Vec<Vec<u8>>> {
        let mut outputs = Vec::new();
        for example in EXAMPLES {
            let output = self.run(
                &installed.binary,
                &[
                    "--agent".into(),
                    "inspect".into(),
                    examples.join(example).into_os_string(),
                ],
            )?;
            let result = agent_result(&output)?;
            require(
                result
                    .get("pages")
                    .and_then(Value::as_u64)
                    .is_some_and(|pages| pages > 0),
                "LIFECYCLE_INSPECT",
                "Installed executable did not inspect a packaged example",
            )?;
            outputs.push(output.stdout);
        }
        Ok(outputs)
    }
}

fn install(archive: &Path, root: &Path) -> Result<Installed> {
    verify_sidecar(archive)?;
    let manifest = verify_archive(archive)?;
    let directory = root.join(basename(&manifest.version, &manifest.target)?);
    let (binary, manifest) = extract_verified(archive, &directory)?;
    Ok(Installed {
        manifest,
        directory,
        binary,
    })
}

fn unchanged(installed: &Installed) -> Result<()> {
    for record in &installed.manifest.files {
        let path = installed.directory.join(&record.path);
        require(
            sha256_file(&path)? == record.sha256,
            "LIFECYCLE_INSTALLATION_CHANGED",
            "An installed file differs from its package manifest",
        )?;
    }
    Ok(())
}

fn empty(directory: &Path) -> Result<bool> {
    Ok(fs::read_dir(directory)?.next().is_none())
}

fn isolated_state(state: &Path) -> Result<BTreeMap<OsString, OsString>> {
    let mut environment = isolated_environment()?;
    for name in STATE_DIRECTORIES {
        fs::create_dir(state.join(name))?;
    }
    let assignments: [(&str, &str); 10] = [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("TMPDIR", "temp"),
        ("TEMP", "temp"),
        ("TMP", "temp"),
        ("APPDATA", "appdata"),
        ("LOCALAPPDATA", "appdata"),
    ];
    for (variable, directory) in assignments {
        environment.insert(variable.into(), state.join(directory).into_os_string());
    }
    Ok(environment)
}

fn release(manifest: &Manifest, archive: &Path) -> Result<Release> {
    Ok(Release {
        version: manifest.version.clone(),
        revision: manifest.revision.clone(),
        archive_sha256: sha256_file(archive)?,
    })
}

struct Recorder {
    checks: Vec<Check>,
}

impl Recorder {
    fn record<T>(&mut self, name: &str, elapsed_ms: u64, result: Result<T>) -> Result<T> {
        self.checks.push(Check {
            name: name.into(),
            passed: result.is_ok(),
            error_code: result.as_ref().err().map(|error| error.code.to_owned()),
            elapsed_ms,
        });
        result
    }
}

struct Plan<'a> {
    previous: &'a Path,
    candidate: &'a Path,
    root: &'a Path,
}

impl<R: Runner> Lifecycle<'_, R> {
    fn step<T>(
        &mut self,
        recorder: &mut Recorder,
        index: usize,
        action: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.elapsed_ms = 0;
        let result = action(self);
        recorder.record(LIFECYCLE_CHECKS[index], self.elapsed_ms, result)
    }

    fn execute(
        &mut self,
        plan: &Plan<'_>,
        recorder: &mut Recorder,
        compatibility: &mut Option<Compatibility>,
    ) -> Result<()> {
        let (previous, previous_capabilities) = self.step(recorder, 0, |lifecycle| {
            let installed = install(plan.previous, plan.root)?;
            lifecycle.version(&installed)?;
            let capabilities = lifecycle.capabilities(&installed)?;
            Ok((installed, capabilities))
        })?;
        let examples = previous.directory.join("examples");
        let baseline = self.step(recorder, 1, |lifecycle| {
            lifecycle.inspect_examples(&previous, &examples)
        })?;
        let candidate = self.step(recorder, 2, |_| {
            let installed = install(plan.candidate, plan.root)?;
            require(
                installed.directory != previous.directory,
                "LIFECYCLE_SAME_DIRECTORY",
                "Side-by-side installations need distinct directories",
            )?;
            Ok(installed)
        })?;
        self.step(recorder, 3, |_| unchanged(&previous))?;
        let candidate_capabilities = self.step(recorder, 4, |lifecycle| {
            lifecycle.version(&candidate)?;
            lifecycle.capabilities(&candidate)
        })?;
        self.step(recorder, 5, |lifecycle| {
            lifecycle
                .inspect_examples(&candidate, &examples)
                .map(|_| ())
        })?;
        self.step(recorder, 6, |_| {
            let result = compare_capabilities(
                &previous_capabilities,
                &candidate_capabilities,
                &previous.manifest,
                &candidate.manifest,
            )?;
            let acceptable = !result.breaking || result.breaking_change_declared;
            *compatibility = Some(result);
            require(
                acceptable,
                "UNDECLARED_BREAKING_CHANGE",
                "The candidate breaks the previous contract without a breaking version change",
            )
        })?;
        self.step(recorder, 7, |lifecycle| {
            lifecycle.version(&previous)?;
            let restored = lifecycle.inspect_examples(&previous, &examples)?;
            require(
                restored == baseline,
                "LIFECYCLE_ROLLBACK_CHANGED",
                "The previous version answers differently after the candidate was installed",
            )
        })?;
        self.step(recorder, 8, |lifecycle| {
            fs::remove_dir_all(&candidate.directory)?;
            unchanged(&previous)?;
            lifecycle.version(&previous)
        })?;
        self.step(recorder, 9, |lifecycle| {
            for name in STATE_DIRECTORIES {
                require(
                    empty(&lifecycle.state.join(name))?,
                    "LIFECYCLE_EXTERNAL_STATE",
                    "DocSight wrote state outside its installation directory",
                )?;
            }
            Ok(())
        })
    }
}

pub fn verify_lifecycle_with<R: Runner>(
    previous_archive: &Path,
    candidate_archive: &Path,
    host: &str,
    runner: &mut R,
) -> Result<LifecycleReceipt> {
    let previous_manifest = verify_archive(previous_archive)?;
    let candidate_manifest = verify_archive(candidate_archive)?;
    require(
        previous_manifest.target == host && candidate_manifest.target == host,
        "LIFECYCLE_HOST_MISMATCH",
        "Both packages must target the native host",
    )?;
    require(
        is_update(&previous_manifest.version, &candidate_manifest.version)?,
        "NOT_AN_UPDATE",
        "The candidate version must be newer than the previous version",
    )?;
    let previous = release(&previous_manifest, previous_archive)?;
    let candidate = release(&candidate_manifest, candidate_archive)?;
    let temporary = tempfile::tempdir()?;
    let base = temporary.path().canonicalize()?;
    let root = base.join("installations");
    let state = base.join("state");
    let work = base.join("work");
    for directory in [&root, &state, &work] {
        fs::create_dir(directory)?;
    }
    let mut lifecycle = Lifecycle {
        runner,
        environment: isolated_state(&state)?,
        state,
        work,
        elapsed_ms: 0,
    };
    let mut recorder = Recorder { checks: Vec::new() };
    let mut compatibility = None;
    let outcome = lifecycle.execute(
        &Plan {
            previous: previous_archive,
            candidate: candidate_archive,
            root: &root,
        },
        &mut recorder,
        &mut compatibility,
    );
    require(
        sha256_file(previous_archive)? == previous.archive_sha256
            && sha256_file(candidate_archive)? == candidate.archive_sha256,
        "ARCHIVE_CHANGED",
        "An archive changed during lifecycle verification",
    )?;
    let checks = recorder.checks;
    let passed = outcome.is_ok()
        && checks.len() == LIFECYCLE_CHECKS.len()
        && checks.iter().all(|check| check.passed);
    Ok(LifecycleReceipt {
        schema: LIFECYCLE_SCHEMA.into(),
        target: host.into(),
        previous,
        candidate,
        checks,
        compatibility,
        passed,
    })
}
