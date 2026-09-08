use docsight_core::DocsightError;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SANDBOX_CHILD_ENV: &str = "DOCSIGHT_SANDBOX_CHILD";
pub const SANDBOX_READ_PATHS_ENV: &str = "DOCSIGHT_SANDBOX_READ_PATHS";
pub const SANDBOX_WRITE_PATHS_ENV: &str = "DOCSIGHT_SANDBOX_WRITE_PATHS";
pub const SANDBOX_TEMP_PATH_ENV: &str = "DOCSIGHT_SANDBOX_TEMP_PATH";

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

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys {
    use super::{
        SANDBOX_READ_PATHS_ENV, SANDBOX_TEMP_PATH_ENV, SANDBOX_WRITE_PATHS_ENV,
        SandboxLimitsReport, SandboxPolicy,
    };
    use docsight_core::DocsightError;
    use std::os::unix::ffi::OsStrExt;

    pub fn apply_resource_limits(
        policy: &SandboxPolicy,
    ) -> Result<SandboxLimitsReport, DocsightError> {
        let mut report = SandboxLimitsReport {
            memory_enforced: false,
            cpu_enforced: false,
            network_isolated: false,
            filesystem_isolated: false,
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
        install_filesystem_filter()?;
        report.filesystem_isolated = true;

        if !report.memory_enforced
            || !report.cpu_enforced
            || !report.network_isolated
            || !report.filesystem_isolated
        {
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: "failed to enforce memory, CPU, network, or filesystem isolation on this platform".to_owned(),
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

    fn install_filesystem_filter() -> Result<(), DocsightError> {
        let read_paths = parse_paths(SANDBOX_READ_PATHS_ENV)?;
        let write_paths = parse_paths(SANDBOX_WRITE_PATHS_ENV)?;
        let temp_path = std::env::var_os(SANDBOX_TEMP_PATH_ENV).map(std::path::PathBuf::from);
        let handled_access = ACCESS_FS_EXECUTE
            | ACCESS_FS_WRITE_FILE
            | ACCESS_FS_READ_FILE
            | ACCESS_FS_READ_DIR
            | ACCESS_FS_REMOVE_DIR
            | ACCESS_FS_REMOVE_FILE
            | ACCESS_FS_MAKE_CHAR
            | ACCESS_FS_MAKE_DIR
            | ACCESS_FS_MAKE_REG
            | ACCESS_FS_MAKE_SOCK
            | ACCESS_FS_MAKE_FIFO
            | ACCESS_FS_MAKE_BLOCK
            | ACCESS_FS_MAKE_SYM;
        let ruleset = RulesetAttr {
            handled_access_fs: handled_access,
            handled_access_net: 0,
            ..RulesetAttr::default()
        };
        let ruleset_fd = landlock_create_ruleset(&ruleset)?;
        let system_read_paths = ["/bin", "/etc", "/lib", "/lib64", "/sbin", "/usr"];
        for path in system_read_paths {
            add_path_rule(
                ruleset_fd,
                std::path::Path::new(path),
                ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR,
            )?;
        }
        for path in read_paths {
            let path = std::path::PathBuf::from(path);
            let canonical = path
                .canonicalize()
                .map_err(|error| DocsightError::BackendFailure {
                    backend: "sandbox".to_owned(),
                    message: format!(
                        "sandbox read path {} is unavailable: {error}",
                        path.display()
                    ),
                })?;
            if let Some(parent) = canonical.parent() {
                add_path_rule(ruleset_fd, parent, ACCESS_FS_EXECUTE | ACCESS_FS_READ_DIR)?;
            }
            add_path_rule(ruleset_fd, &canonical, ACCESS_FS_READ_FILE)?;
        }
        for path in write_paths {
            let path = std::path::PathBuf::from(path);
            let canonical_parent = path
                .canonicalize()
                .or_else(|_| {
                    path.parent()
                        .unwrap_or(std::path::Path::new("."))
                        .canonicalize()
                })
                .map_err(|error| DocsightError::BackendFailure {
                    backend: "sandbox".to_owned(),
                    message: format!(
                        "sandbox write path {} is unavailable: {error}",
                        path.display()
                    ),
                })?;
            let directory = if canonical_parent.is_dir() {
                canonical_parent
            } else {
                canonical_parent
                    .parent()
                    .ok_or_else(|| DocsightError::BackendFailure {
                        backend: "sandbox".to_owned(),
                        message: format!(
                            "sandbox write path {} has no parent directory",
                            path.display()
                        ),
                    })?
                    .to_path_buf()
            };
            add_path_rule(
                ruleset_fd,
                &directory,
                ACCESS_FS_EXECUTE
                    | ACCESS_FS_READ_DIR
                    | ACCESS_FS_WRITE_FILE
                    | ACCESS_FS_REMOVE_FILE
                    | ACCESS_FS_REMOVE_DIR
                    | ACCESS_FS_MAKE_DIR
                    | ACCESS_FS_MAKE_REG,
            )?;
        }
        if let Some(temp_path) = temp_path {
            add_path_rule(ruleset_fd, &temp_path, handled_access)?;
        }
        if unsafe { libc::syscall(LANDLOCK_RESTRICT_SELF, ruleset_fd, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            close_fd(ruleset_fd);
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: format!("failed to restrict worker filesystem access: {error}"),
            });
        }
        close_fd(ruleset_fd);
        Ok(())
    }

    fn parse_paths(variable: &str) -> Result<Vec<String>, DocsightError> {
        let Some(value) = std::env::var_os(variable) else {
            return Ok(Vec::new());
        };
        serde_json::from_str(
            value
                .to_str()
                .ok_or_else(|| DocsightError::BackendFailure {
                    backend: "sandbox".to_owned(),
                    message: format!("{variable} is not valid UTF-8"),
                })?,
        )
        .map_err(|error| DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: format!("{variable} is invalid: {error}"),
        })
    }

    fn add_path_rule(
        ruleset_fd: i32,
        path: &std::path::Path,
        allowed_access: u64,
    ) -> Result<(), DocsightError> {
        if !path.exists() {
            return Ok(());
        }
        let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: format!("sandbox path {} contains a NUL byte", path.display()),
            }
        })?;
        let fd = unsafe { libc::open(path_bytes.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: format!("failed to open sandbox path {}: {error}", path.display()),
            });
        }
        let rule = PathBeneathAttr {
            allowed_access,
            parent_fd: fd,
        };
        let result = unsafe {
            libc::syscall(
                LANDLOCK_ADD_RULE,
                ruleset_fd,
                LANDLOCK_RULE_TYPE_PATH_BENEATH,
                &rule,
                0,
            )
        };
        close_fd(fd);
        if result != 0 {
            let error = std::io::Error::last_os_error();
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: format!(
                    "failed to add sandbox path rule for {}: {error}",
                    path.display()
                ),
            });
        }
        Ok(())
    }

    fn landlock_create_ruleset(attr: &RulesetAttr) -> Result<i32, DocsightError> {
        let fd = unsafe {
            libc::syscall(
                LANDLOCK_CREATE_RULESET,
                attr,
                std::mem::size_of::<RulesetAttr>(),
                0,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(DocsightError::BackendFailure {
                backend: "sandbox".to_owned(),
                message: format!("Landlock filesystem isolation is unavailable: {error}"),
            });
        }
        Ok(fd as i32)
    }

    fn close_fd(fd: i32) {
        unsafe {
            libc::close(fd);
        }
    }

    const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const LANDLOCK_ADD_RULE: libc::c_long = 445;
    const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
    const LANDLOCK_RULE_TYPE_PATH_BENEATH: u32 = 1;
    const ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
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

    let (read_paths, write_paths) = sandbox_paths(args, extra_env)?;
    let read_json =
        serde_json::to_string(&read_paths).map_err(|error| DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: format!("failed to serialize sandbox read paths: {error}"),
        })?;
    let write_json =
        serde_json::to_string(&write_paths).map_err(|error| DocsightError::BackendFailure {
            backend: "sandbox".to_owned(),
            message: format!("failed to serialize sandbox write paths: {error}"),
        })?;
    cmd.env(SANDBOX_READ_PATHS_ENV, read_json);
    cmd.env(SANDBOX_WRITE_PATHS_ENV, write_json);

    let _temp_guard = if policy.isolated_temp_dir {
        let temp_dir = tempfile::tempdir().map_err(|e| DocsightError::Io {
            path: PathBuf::from("<sandbox-temp>"),
            source: e,
        })?;
        cmd.env("TMPDIR", temp_dir.path());
        cmd.env("TEMP", temp_dir.path());
        cmd.env("TMP", temp_dir.path());
        cmd.env(SANDBOX_TEMP_PATH_ENV, temp_dir.path());
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

fn sandbox_paths(
    args: &[String],
    extra_env: &[(String, String)],
) -> Result<(Vec<String>, Vec<String>), DocsightError> {
    let mut read_paths = infer_read_paths(args);
    let mut write_paths = infer_write_paths(args);
    for (key, value) in extra_env {
        if key == SANDBOX_READ_PATHS_ENV {
            read_paths.extend(serde_json::from_str::<Vec<String>>(value).map_err(|error| {
                DocsightError::BackendFailure {
                    backend: "sandbox".to_owned(),
                    message: format!("{SANDBOX_READ_PATHS_ENV} is invalid: {error}"),
                }
            })?);
        }
        if key == SANDBOX_WRITE_PATHS_ENV {
            write_paths.extend(serde_json::from_str::<Vec<String>>(value).map_err(|error| {
                DocsightError::BackendFailure {
                    backend: "sandbox".to_owned(),
                    message: format!("{SANDBOX_WRITE_PATHS_ENV} is invalid: {error}"),
                }
            })?);
        }
    }
    Ok((normalize_paths(read_paths), normalize_paths(write_paths)))
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
