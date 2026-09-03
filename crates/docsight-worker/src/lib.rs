use docsight_core::DocsightError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub max_memory_bytes: u64,
    pub cpu_timeout_secs: u64,
    pub isolated_temp_dir: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_memory_bytes: 768 * 1024 * 1024,
            cpu_timeout_secs: 30,
            isolated_temp_dir: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerOutput {
    pub exit_code: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn find_worker_binary() -> Result<PathBuf, DocsightError> {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_docsight-worker") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_docsight") {
        if let Some(parent) = Path::new(&path).parent() {
            let candidate = parent.join("docsight-worker");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join("docsight-worker");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(DocsightError::BackendFailure {
        backend: "worker".to_owned(),
        message: "docsight-worker binary was not found in environment or binary directory"
            .to_owned(),
    })
}

pub fn run_in_sandbox(
    worker_exe: Option<&Path>,
    policy: &SandboxPolicy,
    args: &[String],
) -> Result<WorkerOutput, DocsightError> {
    let binary = match worker_exe {
        Some(p) => p.to_path_buf(),
        None => find_worker_binary()?,
    };

    let mut cmd = Command::new(&binary);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let _temp_guard = if policy.isolated_temp_dir {
        let temp_dir = tempfile::tempdir().map_err(|e| DocsightError::Io {
            path: PathBuf::from("<sandbox-temp>"),
            source: e,
        })?;
        cmd.env("TMPDIR", temp_dir.path());
        cmd.env("TEMP", temp_dir.path());
        cmd.env("TMP", temp_dir.path());
        Some(temp_dir)
    } else {
        None
    };

    let mut child = cmd.spawn().map_err(|error| DocsightError::BackendFailure {
        backend: "worker".to_owned(),
        message: format!(
            "failed to spawn isolated worker {}: {error}",
            binary.display()
        ),
    })?;

    let timeout = Duration::from_secs(policy.cpu_timeout_secs);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output =
                    child
                        .wait_with_output()
                        .map_err(|error| DocsightError::BackendFailure {
                            backend: "worker".to_owned(),
                            message: format!("failed to read isolated worker output: {error}"),
                        })?;

                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(signal) = status.signal() {
                        return Err(DocsightError::BackendFailure {
                            backend: "worker".to_owned(),
                            message: format!("isolated worker killed by signal {signal}"),
                        });
                    }
                }

                let exit_code = match status.code() {
                    Some(c) if (0..=255).contains(&c) => c as u8,
                    _ => {
                        return Err(DocsightError::BackendFailure {
                            backend: "worker".to_owned(),
                            message: "isolated worker terminated abnormally with non-standard code"
                                .to_owned(),
                        });
                    }
                };

                return Ok(WorkerOutput {
                    exit_code,
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(DocsightError::BackendFailure {
                        backend: "worker".to_owned(),
                        message: format!(
                            "isolated worker exceeded CPU timeout of {} seconds",
                            policy.cpu_timeout_secs
                        ),
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DocsightError::BackendFailure {
                    backend: "worker".to_owned(),
                    message: format!("failed to wait on isolated worker: {error}"),
                });
            }
        }
    }
}
