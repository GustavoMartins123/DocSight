use docsight_core::DocsightError;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SANDBOX_CHILD_ENV: &str = "DOCSIGHT_SANDBOX_CHILD";

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
}

#[cfg(target_os = "linux")]
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

        install_network_filter()?;
        report.network_isolated = true;

        if !report.memory_enforced || !report.cpu_enforced || !report.network_isolated {
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: "failed to enforce memory, CPU, or network isolation on this platform"
                    .to_owned(),
            });
        }
        Ok(report)
    }

    fn install_network_filter() -> Result<(), DocsightError> {
        let architecture = audit_architecture()?;
        let mut filter = vec![
            statement(0x20, 4),
            jump(0x15, architecture, 1, 0),
            statement(0x06, 0x8000_0000),
            statement(0x20, 0),
        ];
        for syscall in network_syscalls() {
            filter.push(jump(0x15, syscall, 0, 1));
            filter.push(statement(0x06, 0x0005_0000 | u32::from(libc::EPERM as u16)));
        }
        filter.push(statement(0x06, 0x7fff_0000));
        let len = u16::try_from(filter.len()).map_err(|_| DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: "network syscall filter exceeds the platform instruction limit".to_owned(),
        })?;
        let program = libc::sock_fprog {
            len,
            filter: filter.as_mut_ptr(),
        };
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: "failed to disable privilege escalation before sandboxing".to_owned(),
            });
        }
        if unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &program as *const libc::sock_fprog,
            )
        } != 0
        {
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: "failed to install the network syscall filter".to_owned(),
            });
        }
        Ok(())
    }

    fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    fn network_syscalls() -> [u32; 18] {
        [
            libc::SYS_socket as u32,
            libc::SYS_socketpair as u32,
            libc::SYS_connect as u32,
            libc::SYS_bind as u32,
            libc::SYS_listen as u32,
            libc::SYS_accept as u32,
            libc::SYS_accept4 as u32,
            libc::SYS_sendto as u32,
            libc::SYS_recvfrom as u32,
            libc::SYS_sendmsg as u32,
            libc::SYS_recvmsg as u32,
            libc::SYS_shutdown as u32,
            libc::SYS_setsockopt as u32,
            libc::SYS_getsockopt as u32,
            libc::SYS_getpeername as u32,
            libc::SYS_getsockname as u32,
            libc::SYS_sendmmsg as u32,
            libc::SYS_recvmmsg as u32,
        ]
    }

    #[cfg(target_arch = "x86_64")]
    fn audit_architecture() -> Result<u32, DocsightError> {
        Ok(0xc000_003e)
    }

    #[cfg(target_arch = "aarch64")]
    fn audit_architecture() -> Result<u32, DocsightError> {
        Ok(0xc000_00b7)
    }

    #[cfg(target_arch = "x86")]
    fn audit_architecture() -> Result<u32, DocsightError> {
        Ok(0x4000_0003)
    }

    #[cfg(target_arch = "arm")]
    fn audit_architecture() -> Result<u32, DocsightError> {
        Ok(0x4000_0028)
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "x86",
        target_arch = "arm"
    )))]
    fn audit_architecture() -> Result<u32, DocsightError> {
        Err(DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: "network syscall filtering is unavailable for this CPU architecture"
                .to_owned(),
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use super::{SandboxLimitsReport, SandboxPolicy};
    use docsight_core::DocsightError;

    pub fn apply_resource_limits(
        _policy: &SandboxPolicy,
    ) -> Result<SandboxLimitsReport, DocsightError> {
        Err(DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: "sandbox enforcement is unavailable on this platform".to_owned(),
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

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: "isolated worker stdout pipe was not created".to_owned(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: "isolated worker stderr pipe was not created".to_owned(),
        })?;
    let stdout_reader = spawn_pipe_reader(stdout, policy.max_output_bytes)?;
    let stderr_reader = spawn_pipe_reader(stderr, policy.max_output_bytes)?;

    let timeout = Duration::from_secs(policy.cpu_timeout_secs);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = join_pipe_reader(stdout_reader, "stdout")?;
                let stderr = join_pipe_reader(stderr_reader, "stderr")?;
                if stdout.exceeded || stderr.exceeded {
                    return Err(DocsightError::ResourceLimit {
                        resource: "isolated worker output bytes".to_owned(),
                        limit: policy.max_output_bytes,
                    });
                }
                let total_output = stdout
                    .bytes
                    .len()
                    .checked_add(stderr.bytes.len())
                    .ok_or_else(|| DocsightError::ResourceLimit {
                        resource: "isolated worker output bytes".to_owned(),
                        limit: policy.max_output_bytes,
                    })?;
                let total_output =
                    u64::try_from(total_output).map_err(|_| DocsightError::ResourceLimit {
                        resource: "isolated worker output bytes".to_owned(),
                        limit: policy.max_output_bytes,
                    })?;
                if total_output > policy.max_output_bytes {
                    return Err(DocsightError::ResourceLimit {
                        resource: "isolated worker output bytes".to_owned(),
                        limit: policy.max_output_bytes,
                    });
                }

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
                    stdout: stdout.bytes,
                    stderr: stderr.bytes,
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    terminate_and_drain(child, stdout_reader, stderr_reader)?;
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
                terminate_and_drain(child, stdout_reader, stderr_reader)?;
                return Err(DocsightError::BackendFailure {
                    backend: "worker".to_owned(),
                    message: format!("failed to wait on isolated worker: {error}"),
                });
            }
        }
    }
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
        usize::try_from(limit.saturating_add(1)).map_err(|_| DocsightError::ResourceLimit {
            resource: "isolated worker output bytes".to_owned(),
            limit,
        })?;
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
        .map_err(|_| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: format!("isolated worker {stream} reader terminated unexpectedly"),
        })?
        .map_err(|error| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: format!("failed to read isolated worker {stream}: {error}"),
        })
}

fn terminate_and_drain(
    mut child: std::process::Child,
    stdout_reader: JoinHandle<io::Result<BoundedPipeOutput>>,
    stderr_reader: JoinHandle<io::Result<BoundedPipeOutput>>,
) -> Result<(), DocsightError> {
    child
        .kill()
        .map_err(|error| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: format!("failed to terminate isolated worker: {error}"),
        })?;
    child
        .wait()
        .map_err(|error| DocsightError::BackendFailure {
            backend: "worker".to_owned(),
            message: format!("failed to reap isolated worker: {error}"),
        })?;
    join_pipe_reader(stdout_reader, "stdout")?;
    join_pipe_reader(stderr_reader, "stderr")?;
    Ok(())
}
