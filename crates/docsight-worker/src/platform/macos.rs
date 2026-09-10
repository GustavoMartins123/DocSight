#![allow(unsafe_code)]

use super::posix::{apply_rlimits, parse_paths};
use super::sandbox_failure;
use crate::{
    SANDBOX_READ_PATHS_ENV, SANDBOX_TEMP_PATH_ENV, SANDBOX_WRITE_PATHS_ENV, SandboxLimitsReport,
    SandboxPolicy,
};
use docsight_core::DocsightError;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

const SYSTEM_READ_PATHS: [&str; 8] = [
    "/bin",
    "/etc",
    "/Library",
    "/private/etc",
    "/private/var/db/dyld",
    "/sbin",
    "/System",
    "/usr",
];

const SYSTEM_READ_DEVICES: [&str; 3] = ["/dev/null", "/dev/random", "/dev/urandom"];

unsafe extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const libc::c_char,
        flags: u64,
        parameters: *const *const libc::c_char,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

pub fn apply_resource_limits(policy: &SandboxPolicy) -> Result<SandboxLimitsReport, DocsightError> {
    let limits = apply_rlimits(policy);
    let mut report = SandboxLimitsReport {
        memory_enforced: limits.memory_enforced,
        cpu_enforced: limits.cpu_enforced,
        network_isolated: false,
        filesystem_isolated: false,
    };

    install_seatbelt_profile()?;
    report.network_isolated = true;
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

fn install_seatbelt_profile() -> Result<(), DocsightError> {
    let profile = seatbelt_profile()?;
    let profile = CString::new(profile)
        .map_err(|_| sandbox_failure("sandbox profile contains a NUL byte"))?;
    let mut errorbuf: *mut libc::c_char = std::ptr::null_mut();
    let parameters: [*const libc::c_char; 1] = [std::ptr::null()];
    let status = unsafe {
        sandbox_init_with_parameters(profile.as_ptr(), 0, parameters.as_ptr(), &mut errorbuf)
    };
    if status != 0 {
        let detail = if errorbuf.is_null() {
            "unknown sandbox profile error".to_owned()
        } else {
            let message = unsafe { CStr::from_ptr(errorbuf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(errorbuf) };
            message
        };
        return Err(sandbox_failure(format!(
            "failed to install the seatbelt sandbox profile: {detail}"
        )));
    }
    Ok(())
}

fn seatbelt_profile() -> Result<String, DocsightError> {
    let read_paths = parse_paths(SANDBOX_READ_PATHS_ENV)?;
    let write_paths = parse_paths(SANDBOX_WRITE_PATHS_ENV)?;
    let temp_path = std::env::var_os(SANDBOX_TEMP_PATH_ENV).map(PathBuf::from);

    let mut profile = String::from("(version 1)\n(deny default)\n(deny network*)\n");
    profile.push_str("(allow file-read-metadata)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow process-info* (target self))\n");
    profile.push_str("(allow file-read*\n");
    for path in SYSTEM_READ_PATHS {
        profile.push_str(&format!("    (subpath {})\n", quote(path)?));
    }
    for path in SYSTEM_READ_DEVICES {
        profile.push_str(&format!("    (literal {})\n", quote(path)?));
    }
    for path in &read_paths {
        let canonical = canonical_existing(Path::new(path))?;
        profile.push_str(&format!("    (literal {})\n", quote_path(&canonical)?));
    }
    profile.push_str(")\n");

    let mut writable = Vec::new();
    for path in &write_paths {
        writable.push(writable_directory(Path::new(path))?);
    }
    if let Some(temp_path) = temp_path {
        writable.push(canonical_existing(&temp_path)?);
    }
    if !writable.is_empty() {
        profile.push_str("(allow file-read* file-write*\n");
        for path in &writable {
            profile.push_str(&format!("    (subpath {})\n", quote_path(path)?));
        }
        profile.push_str(")\n");
    }
    Ok(profile)
}

fn canonical_existing(path: &Path) -> Result<PathBuf, DocsightError> {
    path.canonicalize().map_err(|error| {
        sandbox_failure(format!(
            "sandbox path {} is unavailable: {error}",
            path.display()
        ))
    })
}

fn writable_directory(path: &Path) -> Result<PathBuf, DocsightError> {
    let canonical = path
        .canonicalize()
        .or_else(|_| path.parent().unwrap_or(Path::new(".")).canonicalize())
        .map_err(|error| {
            sandbox_failure(format!(
                "sandbox write path {} is unavailable: {error}",
                path.display()
            ))
        })?;
    if canonical.is_dir() {
        return Ok(canonical);
    }
    canonical.parent().map(Path::to_path_buf).ok_or_else(|| {
        sandbox_failure(format!(
            "sandbox write path {} has no parent directory",
            path.display()
        ))
    })
}

fn quote_path(path: &Path) -> Result<String, DocsightError> {
    let text = path.to_str().ok_or_else(|| {
        sandbox_failure(format!(
            "sandbox path {} is not valid UTF-8",
            path.display()
        ))
    })?;
    quote(text)
}

fn quote(value: &str) -> Result<String, DocsightError> {
    if value.contains('\n') || value.contains('\0') {
        return Err(sandbox_failure(format!(
            "sandbox path {value} contains a character that cannot be expressed in a profile"
        )));
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}
