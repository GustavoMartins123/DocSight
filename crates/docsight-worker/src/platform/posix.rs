#![allow(unsafe_code)]

use super::{PipeReader, ProcessExit, SpawnRequest, sandbox_failure, worker_failure};
use crate::SandboxPolicy;
use docsight_core::DocsightError;
use std::process::{Command, Stdio};

pub fn apply_address_space_limit(limit_bytes: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: limit_bytes as libc::rlim_t,
        rlim_max: limit_bytes as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) == 0 }
}

pub fn apply_cpu_limit(policy: &SandboxPolicy) -> bool {
    let cpu_hard = policy.cpu_timeout_secs.saturating_add(5);
    let cpu_limit = libc::rlimit {
        rlim_cur: policy.cpu_timeout_secs as libc::rlim_t,
        rlim_max: cpu_hard as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit) == 0 }
}

pub fn parse_paths(variable: &str) -> Result<Vec<String>, DocsightError> {
    let Some(value) = std::env::var_os(variable) else {
        return Ok(Vec::new());
    };
    let value = value
        .to_str()
        .ok_or_else(|| sandbox_failure(format!("{variable} is not valid UTF-8")))?;
    serde_json::from_str(value)
        .map_err(|error| sandbox_failure(format!("{variable} is invalid: {error}")))
}

pub struct IsolatedChild {
    child: std::process::Child,
}

pub fn spawn_isolated(
    request: &SpawnRequest<'_>,
    _policy: &SandboxPolicy,
) -> Result<IsolatedChild, DocsightError> {
    let mut command = Command::new(request.binary);
    command.args(request.args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    for (key, value) in request.child_environment()? {
        command.env(key, value);
    }
    let child = command.spawn().map_err(|error| {
        worker_failure(format!(
            "failed to spawn isolated worker {}: {error}",
            request.binary.display()
        ))
    })?;
    Ok(IsolatedChild { child })
}

impl IsolatedChild {
    pub fn take_stdout(&mut self) -> Option<PipeReader> {
        self.child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as PipeReader)
    }

    pub fn take_stderr(&mut self) -> Option<PipeReader> {
        self.child
            .stderr
            .take()
            .map(|stderr| Box::new(stderr) as PipeReader)
    }

    pub fn try_wait(&mut self) -> Result<Option<ProcessExit>, DocsightError> {
        use std::os::unix::process::ExitStatusExt;
        let status = self.child.try_wait().map_err(|error| {
            worker_failure(format!("failed to wait on isolated worker: {error}"))
        })?;
        let Some(status) = status else {
            return Ok(None);
        };
        if let Some(signal) = status.signal() {
            return Ok(Some(ProcessExit::Signal(signal)));
        }
        let code = status
            .code()
            .ok_or_else(|| worker_failure("isolated worker exited without a status code"))?;
        Ok(Some(ProcessExit::Code(i64::from(code))))
    }

    pub fn terminate(&mut self) -> Result<(), DocsightError> {
        self.child.kill().map_err(|error| {
            worker_failure(format!("failed to terminate isolated worker: {error}"))
        })?;
        self.child
            .wait()
            .map_err(|error| worker_failure(format!("failed to reap isolated worker: {error}")))?;
        Ok(())
    }
}
