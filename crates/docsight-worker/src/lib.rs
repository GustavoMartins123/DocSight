mod platform;

use docsight_core::DocsightError;
use platform::{ProcessExit, SpawnRequest, worker_failure};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SANDBOX_CHILD_ENV: &str = "DOCSIGHT_SANDBOX_CHILD";
pub const SANDBOX_READ_PATHS_ENV: &str = "DOCSIGHT_SANDBOX_READ_PATHS";
pub const SANDBOX_WRITE_PATHS_ENV: &str = "DOCSIGHT_SANDBOX_WRITE_PATHS";
pub const SANDBOX_TEMP_PATH_ENV: &str = "DOCSIGHT_SANDBOX_TEMP_PATH";

pub(crate) const WORKER_BACKEND: &str = "worker";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub max_memory_bytes: u64,
    pub cpu_timeout_secs: u64,
    pub isolated_temp_dir: bool,
    pub max_output_bytes: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_memory_bytes: 768 * 1024 * 1024,
            cpu_timeout_secs: 30,
            isolated_temp_dir: true,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxLimitsReport {
    pub memory_enforced: bool,
    pub cpu_enforced: bool,
    pub network_isolated: bool,
    pub filesystem_isolated: bool,
}

pub fn apply_sandbox_limits_if_child(
    policy: &SandboxPolicy,
) -> Result<Option<SandboxLimitsReport>, DocsightError> {
    if std::env::var_os(SANDBOX_CHILD_ENV).is_none() {
        return Ok(None);
    }
    platform::apply_resource_limits(policy).map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerOutput {
    pub exit_code: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn find_worker_binary() -> Result<PathBuf, DocsightError> {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_docsight-worker") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_docsight")
        && let Some(parent) = Path::new(&path).parent()
    {
        let candidate = parent.join(worker_file_name());
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let candidate = parent.join(worker_file_name());
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(worker_failure(
        "docsight-worker binary was not found in environment or binary directory",
    ))
}

fn worker_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "docsight-worker.exe"
    } else {
        "docsight-worker"
    }
}

pub fn run_in_sandbox(
    worker_exe: Option<&Path>,
    policy: &SandboxPolicy,
    args: &[String],
) -> Result<WorkerOutput, DocsightError> {
    run_in_sandbox_with_env(worker_exe, policy, args, &[])
}

pub fn run_in_sandbox_with_env(
    worker_exe: Option<&Path>,
    policy: &SandboxPolicy,
    args: &[String],
    extra_env: &[(String, String)],
) -> Result<WorkerOutput, DocsightError> {
    let binary = match worker_exe {
        Some(path) => path.to_path_buf(),
        None => find_worker_binary()?,
    };

    let (read_paths, write_paths) = sandbox_paths(args, extra_env)?;

    let temp_guard = if policy.isolated_temp_dir {
        Some(tempfile::tempdir().map_err(|error| DocsightError::Io {
            path: PathBuf::from("<sandbox-temp>"),
            source: error,
        })?)
    } else {
        None
    };

    let mut child = platform::spawn_isolated(
        &SpawnRequest {
            binary: &binary,
            args,
            environment: extra_env,
            read_paths: &read_paths,
            write_paths: &write_paths,
            temp_dir: temp_guard.as_ref().map(tempfile::TempDir::path),
        },
        policy,
    )?;

    let stdout = child
        .take_stdout()
        .ok_or_else(|| worker_failure("isolated worker stdout pipe was not created"))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| worker_failure("isolated worker stderr pipe was not created"))?;
    let stdout_reader = spawn_pipe_reader(stdout, policy.max_output_bytes)?;
    let stderr_reader = spawn_pipe_reader(stderr, policy.max_output_bytes)?;

    let timeout = Duration::from_secs(policy.cpu_timeout_secs);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                let stdout = join_pipe_reader(stdout_reader, "stdout")?;
                let stderr = join_pipe_reader(stderr_reader, "stderr")?;
                return collect_output(exit, stdout, stderr, policy);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    terminate_and_drain(child, stdout_reader, stderr_reader)?;
                    return Err(worker_failure(format!(
                        "isolated worker exceeded CPU timeout of {} seconds",
                        policy.cpu_timeout_secs
                    )));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                terminate_and_drain(child, stdout_reader, stderr_reader)?;
                return Err(error);
            }
        }
    }
}

fn collect_output(
    exit: ProcessExit,
    stdout: BoundedPipeOutput,
    stderr: BoundedPipeOutput,
    policy: &SandboxPolicy,
) -> Result<WorkerOutput, DocsightError> {
    if stdout.exceeded || stderr.exceeded {
        return Err(output_limit(policy.max_output_bytes));
    }
    let total_output = stdout
        .bytes
        .len()
        .checked_add(stderr.bytes.len())
        .ok_or_else(|| output_limit(policy.max_output_bytes))?;
    let total_output =
        u64::try_from(total_output).map_err(|_| output_limit(policy.max_output_bytes))?;
    if total_output > policy.max_output_bytes {
        return Err(output_limit(policy.max_output_bytes));
    }

    let exit_code = match exit {
        #[cfg(unix)]
        ProcessExit::Signal(signal) => {
            return Err(worker_failure(format!(
                "isolated worker killed by signal {signal}"
            )));
        }
        ProcessExit::Code(code) if (0..=255).contains(&code) => code as u8,
        ProcessExit::Code(_) => {
            return Err(worker_failure(
                "isolated worker terminated abnormally with non-standard code",
            ));
        }
    };

    Ok(WorkerOutput {
        exit_code,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn output_limit(limit: u64) -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "isolated worker output bytes".to_owned(),
        limit,
    }
}

fn sandbox_paths(
    args: &[String],
    extra_env: &[(String, String)],
) -> Result<(Vec<String>, Vec<String>), DocsightError> {
    let mut read_paths = infer_read_paths(args);
    let mut write_paths = infer_write_paths(args);
    for (key, value) in extra_env {
        if key == SANDBOX_READ_PATHS_ENV {
            read_paths.extend(declared_paths(SANDBOX_READ_PATHS_ENV, value)?);
        }
        if key == SANDBOX_WRITE_PATHS_ENV {
            write_paths.extend(declared_paths(SANDBOX_WRITE_PATHS_ENV, value)?);
        }
    }
    Ok((normalize_paths(read_paths), normalize_paths(write_paths)))
}

fn declared_paths(variable: &str, value: &str) -> Result<Vec<String>, DocsightError> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|error| platform::sandbox_failure(format!("{variable} is invalid: {error}")))
}

fn infer_read_paths(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|argument| !argument.starts_with('-'))
        .filter_map(|argument| {
            let path = Path::new(argument);
            path.is_file()
                .then(|| path.canonicalize().ok())
                .flatten()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .collect()
}

fn infer_write_paths(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut expects_path = false;
    for argument in args {
        if expects_path {
            paths.push(argument.clone());
            expects_path = false;
            continue;
        }
        if argument == "--out" || argument == "--out-dir" {
            expects_path = true;
        } else if let Some(path) = argument.strip_prefix("--out=") {
            paths.push(path.to_owned());
        } else if let Some(path) = argument.strip_prefix("--out-dir=") {
            paths.push(path.to_owned());
        }
    }
    paths
}

fn normalize_paths(paths: Vec<String>) -> Vec<String> {
    let mut normalized = paths
        .into_iter()
        .filter_map(|path| {
            let path = PathBuf::from(path);
            path.canonicalize()
                .or_else(|_| path.parent().unwrap_or(Path::new(".")).canonicalize())
                .ok()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

struct BoundedPipeOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn spawn_pipe_reader<R>(
    mut reader: R,
    limit: u64,
) -> Result<JoinHandle<io::Result<BoundedPipeOutput>>, DocsightError>
where
    R: Read + Send + 'static,
{
    let storage_limit =
        usize::try_from(limit.saturating_add(1)).map_err(|_| output_limit(limit))?;
    Ok(std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read).map_err(|_| {
                io::Error::other("worker pipe read length is outside the supported range")
            })?;
            total = total
                .checked_add(read_u64)
                .ok_or_else(|| io::Error::other("worker pipe output length overflowed"))?;
            if bytes.len() < storage_limit {
                let remaining = storage_limit - bytes.len();
                let keep = remaining.min(read);
                bytes.extend_from_slice(&buffer[..keep]);
            }
        }
        Ok(BoundedPipeOutput {
            bytes,
            exceeded: total > limit,
        })
    }))
}

fn join_pipe_reader(
    reader: JoinHandle<io::Result<BoundedPipeOutput>>,
    stream: &str,
) -> Result<BoundedPipeOutput, DocsightError> {
    reader
        .join()
        .map_err(|_| {
            worker_failure(format!(
                "isolated worker {stream} reader terminated unexpectedly"
            ))
        })?
        .map_err(|error| {
            worker_failure(format!("failed to read isolated worker {stream}: {error}"))
        })
}

fn terminate_and_drain(
    mut child: platform::IsolatedChild,
    stdout_reader: JoinHandle<io::Result<BoundedPipeOutput>>,
    stderr_reader: JoinHandle<io::Result<BoundedPipeOutput>>,
) -> Result<(), DocsightError> {
    child.terminate()?;
    join_pipe_reader(stdout_reader, "stdout")?;
    join_pipe_reader(stderr_reader, "stderr")?;
    Ok(())
}
