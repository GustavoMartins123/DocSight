#![allow(unsafe_code)]

use super::posix::{apply_cpu_limit, parse_paths};
use super::sandbox_failure;
use crate::{
    SANDBOX_READ_PATHS_ENV, SANDBOX_TEMP_PATH_ENV, SANDBOX_WRITE_PATHS_ENV, SandboxLimitsReport,
    SandboxPolicy,
};
use docsight_core::DocsightError;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub fn apply_resource_limits(policy: &SandboxPolicy) -> Result<SandboxLimitsReport, DocsightError> {
    let mut report = SandboxLimitsReport {
        memory_enforced: apply_address_space_limit(policy.max_memory_bytes),
        cpu_enforced: apply_cpu_limit(policy),
        network_isolated: false,
        filesystem_isolated: false,
    };

    install_network_filter()?;
    report.network_isolated = true;
    install_filesystem_filter()?;
    report.filesystem_isolated = true;

    if !report.memory_enforced
        || !report.cpu_enforced
        || !report.network_isolated
        || !report.filesystem_isolated
    {
        return Err(sandbox_failure(
            "failed to enforce memory, CPU, network, or filesystem isolation on this platform",
        ));
    }
    Ok(report)
}

fn apply_address_space_limit(limit_bytes: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: limit_bytes as libc::rlim_t,
        rlim_max: limit_bytes as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) == 0 }
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
    let len = u16::try_from(filter.len()).map_err(|_| {
        sandbox_failure("network syscall filter exceeds the platform instruction limit")
    })?;
    let program = libc::sock_fprog {
        len,
        filter: filter.as_mut_ptr(),
    };
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(sandbox_failure(
            "failed to disable privilege escalation before sandboxing",
        ));
    }
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } != 0
    {
        return Err(sandbox_failure(
            "failed to install the network syscall filter",
        ));
    }
    Ok(())
}

fn install_filesystem_filter() -> Result<(), DocsightError> {
    let read_paths = parse_paths(SANDBOX_READ_PATHS_ENV)?;
    let write_paths = parse_paths(SANDBOX_WRITE_PATHS_ENV)?;
    let temp_path = std::env::var_os(SANDBOX_TEMP_PATH_ENV).map(PathBuf::from);
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
    for path in SYSTEM_READ_PATHS {
        add_path_rule(
            ruleset_fd,
            Path::new(path),
            ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR,
        )?;
    }
    for path in read_paths {
        let path = PathBuf::from(path);
        let canonical = path.canonicalize().map_err(|error| {
            sandbox_failure(format!(
                "sandbox read path {} is unavailable: {error}",
                path.display()
            ))
        })?;
        if let Some(parent) = canonical.parent() {
            add_path_rule(ruleset_fd, parent, ACCESS_FS_EXECUTE | ACCESS_FS_READ_DIR)?;
        }
        add_path_rule(ruleset_fd, &canonical, ACCESS_FS_READ_FILE)?;
    }
    for path in write_paths {
        let path = PathBuf::from(path);
        let canonical_parent = path
            .canonicalize()
            .or_else(|_| path.parent().unwrap_or(Path::new(".")).canonicalize())
            .map_err(|error| {
                sandbox_failure(format!(
                    "sandbox write path {} is unavailable: {error}",
                    path.display()
                ))
            })?;
        let directory = if canonical_parent.is_dir() {
            canonical_parent
        } else {
            canonical_parent
                .parent()
                .ok_or_else(|| {
                    sandbox_failure(format!(
                        "sandbox write path {} has no parent directory",
                        path.display()
                    ))
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
        return Err(sandbox_failure(format!(
            "failed to restrict worker filesystem access: {error}"
        )));
    }
    close_fd(ruleset_fd);
    Ok(())
}

fn add_path_rule(ruleset_fd: i32, path: &Path, allowed_access: u64) -> Result<(), DocsightError> {
    if !path.exists() {
        return Ok(());
    }
    let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        sandbox_failure(format!(
            "sandbox path {} contains a NUL byte",
            path.display()
        ))
    })?;
    let fd = unsafe { libc::open(path_bytes.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return Err(sandbox_failure(format!(
            "failed to open sandbox path {}: {error}",
            path.display()
        )));
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
        return Err(sandbox_failure(format!(
            "failed to add sandbox path rule for {}: {error}",
            path.display()
        )));
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
        return Err(sandbox_failure(format!(
            "Landlock filesystem isolation is unavailable: {error}"
        )));
    }
    Ok(fd as i32)
}

fn close_fd(fd: i32) {
    unsafe {
        libc::close(fd);
    }
}

const SYSTEM_READ_PATHS: [&str; 6] = ["/bin", "/etc", "/lib", "/lib64", "/sbin", "/usr"];
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
    Err(sandbox_failure(
        "network syscall filtering is unavailable for this CPU architecture",
    ))
}
