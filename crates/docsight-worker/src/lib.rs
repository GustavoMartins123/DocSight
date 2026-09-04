use docsight_core::DocsightError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const SANDBOX_CHILD_ENV: &str = "DOCSIGHT_SANDBOX_CHILD";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxLimitsReport {
    pub memory_enforced: bool,
    pub cpu_enforced: bool,
    pub network_isolated: bool,
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod sys {
    use super::{SandboxLimitsReport, SandboxPolicy};
    use docsight_core::DocsightError;

    pub fn apply_resource_limits(
        policy: &SandboxPolicy,
    ) -> Result<SandboxLimitsReport, DocsightError> {
        let mut report = SandboxLimitsReport {
            memory_enforced: false,
            cpu_enforced: false,
            network_isolated: false,
        };

        let memory_limit = libc::rlimit {
            rlim_cur: policy.max_memory_bytes,
            rlim_max: policy.max_memory_bytes,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &memory_limit) } == 0 {
            report.memory_enforced = true;
        }

        let cpu_hard = policy.cpu_timeout_secs.saturating_add(5);
        let cpu_limit = libc::rlimit {
            rlim_cur: policy.cpu_timeout_secs,
            rlim_max: cpu_hard,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit) } == 0 {
            report.cpu_enforced = true;
        }

        if unsafe { libc::unshare(libc::CLONE_NEWUSER) } == 0
            && unsafe { libc::unshare(libc::CLONE_NEWNET) } == 0
        {
            report.network_isolated = true;
        }

        if !report.memory_enforced || !report.cpu_enforced {
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: "failed to enforce memory or CPU limits on this platform".to_owned(),
            });
        }
        Ok(report)
    }
}

#[cfg(not(unix))]
mod sys {
    use super::{SandboxLimitsReport, SandboxPolicy};
    use docsight_core::DocsightError;

    pub fn apply_resource_limits(
        _policy: &SandboxPolicy,
    ) -> Result<SandboxLimitsReport, DocsightError> {
        Ok(SandboxLimitsReport {
            memory_enforced: false,
            cpu_enforced: false,
            network_isolated: false,
        })
    }
}

pub fn apply_sandbox_limits_if_child(
    policy: &SandboxPolicy,
) -> Result<Option<SandboxLimitsReport>, DocsightError> {
    if std::env::var_os(SANDBOX_CHILD_ENV).is_none() {
        return Ok(None);
    }
    sys::apply_resource_limits(policy).map(Some)
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
    run_in_sandbox_with_env(worker_exe, policy, args, &[])
}

pub fn run_in_sandbox_with_env(
    worker_exe: Option<&Path>,
    policy: &SandboxPolicy,
    args: &[String],
    extra_env: &[(String, String)],
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
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

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
