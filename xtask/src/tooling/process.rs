use super::common::{Result, ToolError, require};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    Timeout,
    OutputLimit,
}

#[derive(Clone, Debug)]
pub struct ProcessResult {
    pub returncode: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed_ms: u64,
    pub termination: Option<Termination>,
}

#[derive(Clone, Debug)]
pub struct ProcessLimits {
    pub timeout: Duration,
    pub output_bytes: usize,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            output_bytes: 4_194_304,
        }
    }
}

impl ProcessLimits {
    pub fn validate(&self) -> Result<()> {
        require(
            self.timeout >= Duration::from_millis(1) && self.timeout <= Duration::from_secs(86_400),
            "INVALID_NUMBER",
            "Process timeout must be between one millisecond and one day",
        )?;
        require(
            (1..=268_435_456).contains(&self.output_bytes),
            "INVALID_INTEGER",
            "Process output limit is outside its permitted range",
        )
    }
}

pub fn isolated_environment() -> Result<BTreeMap<OsString, OsString>> {
    let mut environment = BTreeMap::new();
    for name in ["SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.into(), value);
        }
    }
    #[cfg(windows)]
    let path = std::env::var_os("SYSTEMROOT")
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .into_os_string()
        })
        .ok_or_else(|| {
            ToolError::new(
                "MISSING_SYSTEMROOT",
                "Windows execution requires SYSTEMROOT",
            )
        })?;
    #[cfg(not(windows))]
    let path: OsString = "/usr/bin:/bin".into();
    environment.insert("PATH".into(), path);
    for (key, value) in [("LANG", "C"), ("LC_ALL", "C"), ("NO_COLOR", "1")] {
        environment.insert(key.into(), value.into());
    }
    Ok(environment)
}

fn capture<R: Read + Send + 'static>(
    mut stream: R,
    budget: usize,
    used: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<std::io::Result<Vec<u8>>>> {
    thread::Builder::new().spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let length = match stream.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if length == 0 {
                break;
            }
            let previous = used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    Some(value.saturating_add(length))
                })
                .unwrap_or_else(|value| value);
            let allowed = length.min(budget.saturating_sub(previous));
            output.extend_from_slice(&buffer[..allowed]);
            if allowed != length {
                exceeded.store(true, Ordering::Release);
                break;
            }
        }
        Ok(output)
    })
}

struct ChildGuard {
    child: Child,
    group: platform::Group,
    terminated: bool,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.terminated {
            let _cleanup = self.group.terminate(&mut self.child);
        }
        let _reap = self.child.wait();
    }
}

pub fn run_bounded<S: AsRef<OsStr>>(
    arguments: &[S],
    cwd: &Path,
    limits: &ProcessLimits,
    environment: Option<&BTreeMap<OsString, OsString>>,
) -> Result<ProcessResult> {
    limits.validate()?;
    require(
        !arguments.is_empty() && !arguments[0].as_ref().is_empty(),
        "INVALID_COMMAND",
        "A nonempty argument vector is required",
    )?;
    let mut command = Command::new(arguments[0].as_ref());
    command
        .args(arguments[1..].iter().map(AsRef::as_ref))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(environment) = environment {
        command.env_clear().envs(environment);
    }
    platform::configure(&mut command);
    let start = Instant::now();
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ToolError::new(
            "EXECUTABLE_UNAVAILABLE",
            "Required executable is unavailable",
        ),
        _ => ToolError::new("PROCESS_UNAVAILABLE", "Process could not start"),
    })?;
    let group = match platform::Group::attach(&child) {
        Ok(group) => group,
        Err(error) => {
            let _kill = child.kill();
            let _wait = child.wait();
            return Err(error);
        }
    };
    let mut process = ChildGuard {
        child,
        group,
        terminated: false,
    };
    let out = process
        .child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new("PROCESS_IO", "Missing stdout pipe"))?;
    let err = process
        .child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new("PROCESS_IO", "Missing stderr pipe"))?;
    let used = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = capture(
        out,
        limits.output_bytes,
        Arc::clone(&used),
        Arc::clone(&exceeded),
    )?;
    let stderr = capture(err, limits.output_bytes, used, Arc::clone(&exceeded))?;
    let mut termination = None;
    let status = loop {
        if exceeded.load(Ordering::Acquire) {
            termination = Some(Termination::OutputLimit);
            break None;
        }
        if start.elapsed() >= limits.timeout {
            termination = Some(Termination::Timeout);
            break None;
        }
        if let Some(status) = process.child.try_wait()? {
            break Some(status);
        }
        thread::sleep(Duration::from_millis(5));
    };
    process.group.terminate(&mut process.child)?;
    process.terminated = true;
    let status = match status {
        Some(status) => status,
        None => process.child.wait()?,
    };
    let stdout = stdout
        .join()
        .map_err(|_| ToolError::new("PROCESS_IO", "Stdout reader failed"))??;
    let stderr = stderr
        .join()
        .map_err(|_| ToolError::new("PROCESS_IO", "Stderr reader failed"))??;
    if exceeded.load(Ordering::Acquire) {
        termination = Some(Termination::OutputLimit);
    }
    let elapsed_ms = u64::try_from(start.elapsed().as_millis())
        .map_err(|_| ToolError::new("INVALID_NUMBER", "Process duration overflow"))?;
    Ok(ProcessResult {
        returncode: exit_code(status),
        stdout,
        stderr,
        elapsed_ms,
        termination,
    })
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .map(i64::from)
        .unwrap_or_else(|| -i64::from(status.signal().unwrap_or(1)))
}
#[cfg(windows)]
fn exit_code(status: ExitStatus) -> i64 {
    status
        .code()
        .map(|code| i64::from(u32::from_ne_bytes(code.to_ne_bytes())))
        .unwrap_or(-1)
}
#[cfg(not(any(unix, windows)))]
fn exit_code(status: ExitStatus) -> i64 {
    status.code().map(i64::from).unwrap_or(-1)
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    pub fn configure(command: &mut Command) {
        command.process_group(0);
    }
    pub struct Group {
        pid: i32,
    }
    impl Group {
        pub fn attach(child: &Child) -> Result<Self> {
            Ok(Self {
                pid: i32::try_from(child.id())
                    .map_err(|_| ToolError::new("PROCESS_GROUP", "Process identifier overflow"))?,
            })
        }
        pub fn terminate(&self, child: &mut Child) -> Result<()> {
            let code = unsafe { kill(-self.pid, 9) };
            if code != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(3) {
                    child.kill()?;
                    return Err(ToolError::new(
                        "PROCESS_GROUP",
                        "Cannot terminate process group",
                    ));
                }
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    pub fn configure(_: &mut Command) {}
    pub struct Group {
        handle: HANDLE,
    }
    impl Group {
        pub fn attach(child: &Child) -> Result<Self> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            require(
                !handle.is_null(),
                "PROCESS_GROUP",
                "Cannot create process job",
            )?;
            let group = Self { handle };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of_val(&limits))
                .map_err(|_| ToolError::new("PROCESS_GROUP", "Job size overflow"))?;
            let configured = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size,
                )
            };
            require(
                configured != 0,
                "PROCESS_GROUP",
                "Cannot configure process job",
            )?;
            require(
                unsafe { AssignProcessToJobObject(handle, child.as_raw_handle().cast()) } != 0,
                "PROCESS_GROUP",
                "Cannot attach process to its job",
            )?;
            Ok(group)
        }
        pub fn terminate(&self, _: &mut Child) -> Result<()> {
            require(
                unsafe { TerminateJobObject(self.handle, 1) } != 0,
                "PROCESS_GROUP",
                "Cannot terminate process job",
            )
        }
    }
    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub fn configure(_: &mut Command) {}
    pub struct Group;
    impl Group {
        pub fn attach(_: &Child) -> Result<Self> {
            Err(ToolError::new(
                "UNSUPPORTED_HOST",
                "Process groups require Unix or Windows",
            ))
        }
        pub fn terminate(&self, child: &mut Child) -> Result<()> {
            child.kill().map_err(Into::into)
        }
    }
}

pub trait Runner {
    fn run(
        &mut self,
        arguments: &[OsString],
        cwd: &Path,
        limits: &ProcessLimits,
        environment: Option<&BTreeMap<OsString, OsString>>,
    ) -> Result<ProcessResult>;
}

pub struct NativeRunner;
impl Runner for NativeRunner {
    fn run(
        &mut self,
        arguments: &[OsString],
        cwd: &Path,
        limits: &ProcessLimits,
        environment: Option<&BTreeMap<OsString, OsString>>,
    ) -> Result<ProcessResult> {
        run_bounded(arguments, cwd, limits, environment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FailedRead;
    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other(
                "private input path must not appear in public errors",
            ))
        }
    }
    #[test]
    fn reader_io_failures_propagate_without_disclosing_the_source_error() -> Result<()> {
        let handle = capture(
            FailedRead,
            128,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        )?;
        let result = handle
            .join()
            .map_err(|_| ToolError::new("TEST_THREAD", "Test reader did not complete"))?;
        let error = result
            .err()
            .ok_or_else(|| ToolError::new("TEST_READER", "Test reader unexpectedly succeeded"))?;
        let public = ToolError::from(error);
        assert_eq!(public.code, "IO_ERROR");
        assert!(!public.message.contains("private"));
        Ok(())
    }
}
