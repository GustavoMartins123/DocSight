#![allow(unsafe_code)]

use super::{PipeReader, ProcessExit, SpawnRequest, sandbox_failure, worker_failure};
use crate::{SandboxLimitsReport, SandboxPolicy};
use docsight_core::DocsightError;
use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ACCESS_MODE, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS,
    SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, FreeSid, GetTokenInformation,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    TOKEN_QUERY, TokenCapabilities, TokenIsAppContainer,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ReleaseMutex, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

const CONTAINER_NAME_PREFIX: &str = "DocSight.Sandbox";
const ACCESS_CONTROL_LOCK_NAME: &str = r"Local\DocSight.Sandbox.AccessControl";
const ACCESS_CONTROL_LOCK_TIMEOUT_MS: u32 = 30_000;
const HUNDRED_NANOSECONDS_PER_SECOND: u64 = 10_000_000;
const REQUIRED_JOB_LIMITS: u32 = JOB_OBJECT_LIMIT_PROCESS_MEMORY
    | JOB_OBJECT_LIMIT_PROCESS_TIME
    | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
const WRITE_ACCESS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;
const DIRECTORY_INHERITANCE: u32 = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;

static CONTAINER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn apply_resource_limits(policy: &SandboxPolicy) -> Result<SandboxLimitsReport, DocsightError> {
    let confinement = query_container_confinement()?;
    if !confinement.app_container {
        return Err(sandbox_failure(
            "the isolated worker is not running inside an AppContainer",
        ));
    }
    if confinement.capability_count != 0 {
        return Err(sandbox_failure(format!(
            "the isolated worker AppContainer declares {} capabilities instead of none",
            confinement.capability_count
        )));
    }
    let job = query_job_limits()?;
    let expected_cpu = policy
        .cpu_timeout_secs
        .saturating_mul(HUNDRED_NANOSECONDS_PER_SECOND);
    let report = SandboxLimitsReport {
        memory_enforced: job.memory_limit >= 1 && job.memory_limit <= policy.max_memory_bytes,
        cpu_enforced: job.cpu_limit >= 1 && job.cpu_limit <= expected_cpu,
        network_isolated: true,
        filesystem_isolated: true,
    };
    if job.limit_flags & REQUIRED_JOB_LIMITS != REQUIRED_JOB_LIMITS {
        return Err(sandbox_failure(
            "the isolated worker job object does not enforce memory, CPU, and process limits",
        ));
    }
    if !report.memory_enforced || !report.cpu_enforced {
        return Err(sandbox_failure(
            "the isolated worker job object limits do not match the requested sandbox policy",
        ));
    }
    Ok(report)
}

struct ContainerConfinement {
    app_container: bool,
    capability_count: u32,
}

fn query_container_confinement() -> Result<ContainerConfinement, DocsightError> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(sandbox_failure(format!(
            "failed to open the isolated worker process token: {}",
            last_error()
        )));
    }
    let token = OwnedHandle::new(token);
    let mut app_container: u32 = 0;
    let mut returned: u32 = 0;
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenIsAppContainer,
            &mut app_container as *mut u32 as *mut c_void,
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4),
            &mut returned,
        )
    } == 0
    {
        return Err(sandbox_failure(format!(
            "failed to query the isolated worker AppContainer state: {}",
            last_error()
        )));
    }
    let mut capability_bytes: u32 = 0;
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenCapabilities,
            std::ptr::null_mut(),
            0,
            &mut capability_bytes,
        )
    };
    let mut buffer = vec![0u8; usize::try_from(capability_bytes).unwrap_or_default().max(4)];
    let capacity = u32::try_from(buffer.len())
        .map_err(|_| sandbox_failure("the AppContainer capability list is too large"))?;
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenCapabilities,
            buffer.as_mut_ptr() as *mut c_void,
            capacity,
            &mut capability_bytes,
        )
    } == 0
    {
        return Err(sandbox_failure(format!(
            "failed to query the isolated worker AppContainer capabilities: {}",
            last_error()
        )));
    }
    let count_bytes: [u8; 4] = buffer
        .get(..4)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| sandbox_failure("the AppContainer capability list is malformed"))?;
    Ok(ContainerConfinement {
        app_container: app_container != 0,
        capability_count: u32::from_ne_bytes(count_bytes),
    })
}

struct JobLimits {
    memory_limit: u64,
    cpu_limit: u64,
    limit_flags: u32,
}

fn query_job_limits() -> Result<JobLimits, DocsightError> {
    let mut in_job = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) } == 0 {
        return Err(sandbox_failure(format!(
            "failed to query the isolated worker job membership: {}",
            last_error()
        )));
    }
    if in_job == 0 {
        return Err(sandbox_failure(
            "the isolated worker is not assigned to a job object",
        ));
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let mut returned = 0u32;
    let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| sandbox_failure("the job object limit structure is too large"))?;
    if unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            &mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *mut c_void,
            size,
            &mut returned,
        )
    } == 0
    {
        return Err(sandbox_failure(format!(
            "failed to query the isolated worker job limits: {}",
            last_error()
        )));
    }
    Ok(JobLimits {
        memory_limit: u64::try_from(limits.ProcessMemoryLimit).unwrap_or(u64::MAX),
        cpu_limit: u64::try_from(limits.BasicLimitInformation.PerProcessUserTimeLimit)
            .unwrap_or_default(),
        limit_flags: limits.BasicLimitInformation.LimitFlags,
    })
}

pub struct IsolatedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    job: OwnedHandle,
    standard_input: OwnedHandle,
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
    confinement: Confinement,
}

pub fn spawn_isolated(
    request: &SpawnRequest<'_>,
    policy: &SandboxPolicy,
) -> Result<IsolatedChild, DocsightError> {
    let mut confinement = Confinement::new(AppContainer::create()?);
    confinement.grant(request.binary, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, 0)?;
    for path in request.read_paths {
        confinement.grant(Path::new(path), FILE_GENERIC_READ, 0)?;
    }
    for path in request.write_paths {
        confinement.grant(
            &writable_directory(Path::new(path))?,
            WRITE_ACCESS,
            DIRECTORY_INHERITANCE,
        )?;
    }
    if let Some(temp_dir) = request.temp_dir {
        confinement.grant(temp_dir, WRITE_ACCESS, DIRECTORY_INHERITANCE)?;
    }

    let mut child = ConfinedProcess::start(request, policy, confinement.container())?;
    let stdout = child.take_stdout();
    let stderr = child.take_stderr();
    Ok(IsolatedChild {
        process: child.process,
        thread: child.thread,
        job: child.job,
        standard_input: child.standard_input,
        stdout,
        stderr,
        confinement,
    })
}

struct Confinement {
    container: AppContainer,
    granted: Vec<PathBuf>,
}

impl Confinement {
    fn new(container: AppContainer) -> Self {
        Self {
            container,
            granted: Vec::new(),
        }
    }

    fn container(&self) -> &AppContainer {
        &self.container
    }

    fn grant(&mut self, path: &Path, access: u32, inheritance: u32) -> Result<(), DocsightError> {
        set_path_access(path, self.container.sid(), access, inheritance, SET_ACCESS)?;
        self.granted.push(path.to_path_buf());
        Ok(())
    }
}

impl Confinement {
    fn revoke(&mut self) {
        for path in std::mem::take(&mut self.granted) {
            let _ = set_path_access(&path, self.container.sid(), 0, 0, REVOKE_ACCESS);
        }
        self.container.release();
    }
}

impl Drop for Confinement {
    fn drop(&mut self) {
        self.revoke();
    }
}

struct ConfinedProcess {
    process: OwnedHandle,
    thread: OwnedHandle,
    job: OwnedHandle,
    standard_input: OwnedHandle,
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
}

impl ConfinedProcess {
    fn start(
        request: &SpawnRequest<'_>,
        policy: &SandboxPolicy,
        container: &AppContainer,
    ) -> Result<Self, DocsightError> {
        let (stdout_reader, stdout_writer) = create_pipe()?;
        let (stderr_reader, stderr_writer) = create_pipe()?;
        let standard_input = open_null_device()?;

        let mut attributes = ProcThreadAttributeList::with_capacity(1)?;
        let mut capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: container.sid(),
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        attributes.set_security_capabilities(&mut capabilities)?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
            .map_err(|_| sandbox_failure("the process startup structure is too large"))?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = standard_input.raw();
        startup.StartupInfo.hStdOutput = stdout_writer.raw();
        startup.StartupInfo.hStdError = stderr_writer.raw();
        startup.lpAttributeList = attributes.raw();

        let mut command_line = command_line(request.binary, request.args)?;
        let mut environment = environment_block(&request.child_environment()?)?;
        let mut information = PROCESS_INFORMATION::default();
        let created = unsafe {
            CreateProcessW(
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                environment.as_mut_ptr() as *const c_void,
                std::ptr::null(),
                &startup.StartupInfo,
                &mut information,
            )
        };
        if created == 0 {
            return Err(worker_failure(format!(
                "failed to spawn isolated worker {}: {}",
                request.binary.display(),
                last_error()
            )));
        }
        let process = OwnedHandle::new(information.hProcess);
        let thread = OwnedHandle::new(information.hThread);

        let job = create_job(policy)?;
        if unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } == 0 {
            let error = last_error();
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(sandbox_failure(format!(
                "failed to assign the isolated worker to its job object: {error}"
            )));
        }
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let error = last_error();
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(worker_failure(format!(
                "failed to resume the isolated worker: {error}"
            )));
        }
        drop(stdout_writer);
        drop(stderr_writer);
        Ok(Self {
            process,
            thread,
            job,
            standard_input,
            stdout: Some(unsafe {
                std::fs::File::from_raw_handle(stdout_reader.into_raw().cast())
            }),
            stderr: Some(unsafe {
                std::fs::File::from_raw_handle(stderr_reader.into_raw().cast())
            }),
        })
    }

    fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<std::fs::File> {
        self.stderr.take()
    }
}

impl IsolatedChild {
    pub fn take_stdout(&mut self) -> Option<PipeReader> {
        self.stdout.take().map(|pipe| Box::new(pipe) as PipeReader)
    }

    pub fn take_stderr(&mut self) -> Option<PipeReader> {
        self.stderr.take().map(|pipe| Box::new(pipe) as PipeReader)
    }

    pub fn try_wait(&mut self) -> Result<Option<ProcessExit>, DocsightError> {
        let status = unsafe { WaitForSingleObject(self.process.raw(), 0) };
        if status == WAIT_TIMEOUT {
            return Ok(None);
        }
        if status != WAIT_OBJECT_0 {
            return Err(worker_failure(format!(
                "failed to wait on isolated worker: {}",
                last_error()
            )));
        }
        let mut code: u32 = 0;
        if unsafe { GetExitCodeProcess(self.process.raw(), &mut code) } == 0 {
            return Err(worker_failure(format!(
                "failed to read the isolated worker exit code: {}",
                last_error()
            )));
        }
        Ok(Some(ProcessExit::Code(i64::from(code))))
    }

    pub fn terminate(&mut self) -> Result<(), DocsightError> {
        if unsafe { TerminateProcess(self.process.raw(), 1) } == 0 {
            return Err(worker_failure(format!(
                "failed to terminate isolated worker: {}",
                last_error()
            )));
        }
        if unsafe { WaitForSingleObject(self.process.raw(), u32::MAX) } != WAIT_OBJECT_0 {
            return Err(worker_failure(format!(
                "failed to reap isolated worker: {}",
                last_error()
            )));
        }
        Ok(())
    }
}

impl Drop for IsolatedChild {
    fn drop(&mut self) {
        self.stdout.take();
        self.stderr.take();
        self.standard_input.close();
        self.thread.close();
        self.process.close();
        self.job.close();
        self.confinement.revoke();
    }
}

fn create_profile(name: &[u16]) -> Result<PSID, i32> {
    let mut sid: PSID = std::ptr::null_mut();
    let result = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            name.as_ptr(),
            name.as_ptr(),
            std::ptr::null(),
            0,
            &mut sid,
        )
    };
    if result < 0 {
        return Err(result);
    }
    Ok(sid)
}

fn profile_already_exists() -> i32 {
    i32::from_ne_bytes((0x8007_0000u32 | ERROR_ALREADY_EXISTS).to_ne_bytes())
}

struct AppContainer {
    name: Vec<u16>,
    sid: PSID,
}

impl AppContainer {
    fn create() -> Result<Self, DocsightError> {
        let sequence = CONTAINER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = wide(OsStr::new(&format!(
            "{CONTAINER_NAME_PREFIX}.{}.{sequence}",
            std::process::id()
        )));
        match create_profile(&name) {
            Ok(sid) => Ok(Self { name, sid }),
            Err(result) if result == profile_already_exists() => {
                unsafe { DeleteAppContainerProfile(name.as_ptr()) };
                let sid = create_profile(&name).map_err(|result| {
                    sandbox_failure(format!(
                        "failed to replace a stale isolated worker AppContainer profile: error {result:#x}"
                    ))
                })?;
                Ok(Self { name, sid })
            }
            Err(result) => Err(sandbox_failure(format!(
                "failed to create the isolated worker AppContainer profile: error {result:#x}"
            ))),
        }
    }

    fn sid(&self) -> PSID {
        self.sid
    }

    fn release(&mut self) {
        if self.sid.is_null() {
            return;
        }
        unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        unsafe { FreeSid(self.sid) };
        self.sid = std::ptr::null_mut();
    }
}

struct OwnedHandle {
    handle: HANDLE,
}

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self { handle }
    }

    fn raw(&self) -> HANDLE {
        self.handle
    }

    fn into_raw(mut self) -> HANDLE {
        let handle = self.handle;
        self.handle = std::ptr::null_mut();
        handle
    }

    fn close(&mut self) {
        if self.handle.is_null() || self.handle == INVALID_HANDLE_VALUE {
            return;
        }
        unsafe { CloseHandle(self.handle) };
        self.handle = std::ptr::null_mut();
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        self.close();
    }
}

struct ProcThreadAttributeList {
    buffer: Vec<usize>,
    initialized: bool,
}

impl ProcThreadAttributeList {
    fn with_capacity(attributes: u32) -> Result<Self, DocsightError> {
        let mut size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), attributes, 0, &mut size)
        };
        if size == 0 {
            return Err(sandbox_failure(format!(
                "failed to size the isolated worker attribute list: {}",
                last_error()
            )));
        }
        let words = size / std::mem::size_of::<usize>() + 1;
        let mut list = Self {
            buffer: vec![0usize; words],
            initialized: false,
        };
        if unsafe { InitializeProcThreadAttributeList(list.raw(), attributes, 0, &mut size) } == 0 {
            return Err(sandbox_failure(format!(
                "failed to initialize the isolated worker attribute list: {}",
                last_error()
            )));
        }
        list.initialized = true;
        Ok(list)
    }

    fn raw(&mut self) -> *mut c_void {
        self.buffer.as_mut_ptr() as *mut c_void
    }

    fn set_security_capabilities(
        &mut self,
        capabilities: &mut SECURITY_CAPABILITIES,
    ) -> Result<(), DocsightError> {
        let size = std::mem::size_of::<SECURITY_CAPABILITIES>();
        if unsafe {
            UpdateProcThreadAttribute(
                self.raw(),
                0,
                usize::try_from(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES).unwrap_or_default(),
                capabilities as *mut SECURITY_CAPABILITIES as *const c_void,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(sandbox_failure(format!(
                "failed to attach the AppContainer identity to the isolated worker: {}",
                last_error()
            )));
        }
        Ok(())
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        let list = self.raw();
        unsafe { DeleteProcThreadAttributeList(list) };
    }
}

fn create_job(policy: &SandboxPolicy) -> Result<OwnedHandle, DocsightError> {
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(sandbox_failure(format!(
            "failed to create the isolated worker job object: {}",
            last_error()
        )));
    }
    let job = OwnedHandle::new(job);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        REQUIRED_JOB_LIMITS | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.BasicLimitInformation.PerProcessUserTimeLimit = i64::try_from(
        policy
            .cpu_timeout_secs
            .saturating_mul(HUNDRED_NANOSECONDS_PER_SECOND),
    )
    .map_err(|_| sandbox_failure("the sandbox CPU timeout exceeds the platform range"))?;
    limits.ProcessMemoryLimit = usize::try_from(policy.max_memory_bytes)
        .map_err(|_| sandbox_failure("the sandbox memory limit exceeds the platform range"))?;
    let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| sandbox_failure("the job object limit structure is too large"))?;
    if unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
            size,
        )
    } == 0
    {
        return Err(sandbox_failure(format!(
            "failed to apply memory and CPU limits to the isolated worker job object: {}",
            last_error()
        )));
    }
    Ok(job)
}

fn create_pipe() -> Result<(OwnedHandle, OwnedHandle), DocsightError> {
    let attributes = inheritable_attributes();
    let mut reader: HANDLE = std::ptr::null_mut();
    let mut writer: HANDLE = std::ptr::null_mut();
    if unsafe { CreatePipe(&mut reader, &mut writer, &attributes, 0) } == 0 {
        return Err(worker_failure(format!(
            "failed to create an isolated worker output pipe: {}",
            last_error()
        )));
    }
    let reader = OwnedHandle::new(reader);
    let writer = OwnedHandle::new(writer);
    if unsafe { SetHandleInformation(reader.raw(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(worker_failure(format!(
            "failed to restrict inheritance of an isolated worker pipe: {}",
            last_error()
        )));
    }
    Ok((reader, writer))
}

fn open_null_device() -> Result<OwnedHandle, DocsightError> {
    let attributes = inheritable_attributes();
    let path = wide(OsStr::new("NUL"));
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(worker_failure(format!(
            "failed to open the null device for the isolated worker: {}",
            last_error()
        )));
    }
    Ok(OwnedHandle::new(handle))
}

fn inheritable_attributes() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or_default(),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    }
}

fn set_path_access(
    path: &Path,
    sid: PSID,
    access: u32,
    inheritance: u32,
    mode: ACCESS_MODE,
) -> Result<(), DocsightError> {
    let _serialized = AccessControlLock::acquire()?;
    let wide_path = wide(path.as_os_str());
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(sandbox_failure(format!(
            "failed to read the access control list of {}: error {status}",
            path.display()
        )));
    }
    let descriptor = LocalAllocation::new(descriptor);
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    };
    let mut updated: *mut ACL = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut updated) };
    if status != 0 {
        return Err(sandbox_failure(format!(
            "failed to build the sandbox access control list for {}: error {status}",
            path.display()
        )));
    }
    let updated = LocalAllocation::new(updated.cast());
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            updated.raw().cast(),
            std::ptr::null_mut(),
        )
    };
    drop(descriptor);
    if status != 0 {
        return Err(sandbox_failure(format!(
            "failed to grant the isolated worker access to {}: error {status}",
            path.display()
        )));
    }
    Ok(())
}

struct AccessControlLock {
    handle: OwnedHandle,
}

impl AccessControlLock {
    fn acquire() -> Result<Self, DocsightError> {
        let name = wide(OsStr::new(ACCESS_CONTROL_LOCK_NAME));
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(sandbox_failure(format!(
                "failed to open the sandbox access control lock: {}",
                last_error()
            )));
        }
        let handle = OwnedHandle::new(handle);
        match unsafe { WaitForSingleObject(handle.raw(), ACCESS_CONTROL_LOCK_TIMEOUT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { handle }),
            WAIT_TIMEOUT => Err(sandbox_failure(
                "timed out waiting for the sandbox access control lock",
            )),
            _ => Err(sandbox_failure(format!(
                "failed to acquire the sandbox access control lock: {}",
                last_error()
            ))),
        }
    }
}

impl Drop for AccessControlLock {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.handle.raw()) };
        self.handle.close();
    }
}

struct LocalAllocation {
    pointer: *mut c_void,
}

impl LocalAllocation {
    fn new(pointer: *mut c_void) -> Self {
        Self { pointer }
    }

    fn raw(&self) -> *mut c_void {
        self.pointer
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if self.pointer.is_null() {
            return;
        }
        unsafe { LocalFree(self.pointer) };
        self.pointer = std::ptr::null_mut();
    }
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

fn environment_block(overrides: &[(String, String)]) -> Result<Vec<u16>, DocsightError> {
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let overrides: Vec<(OsString, OsString)> = overrides
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    for (key, value) in overrides {
        variables.retain(|(existing, _)| !os_equal_ignore_case(existing, &key));
        variables.push((key, value));
    }
    variables.sort_by_key(|(key, _)| sort_key(key));
    let mut block = Vec::new();
    for (key, value) in variables {
        if key.is_empty() {
            continue;
        }
        let mut entry = key.to_os_string();
        entry.push(OsStr::new("="));
        entry.push(&value);
        let encoded = wide(&entry);
        if encoded.iter().rev().skip(1).any(|unit| *unit == 0) {
            return Err(sandbox_failure(
                "an environment variable contains a NUL character",
            ));
        }
        block.extend(encoded);
    }
    block.push(0);
    Ok(block)
}

fn os_equal_ignore_case(left: &OsStr, right: &OsStr) -> bool {
    sort_key(left) == sort_key(right)
}

fn sort_key(value: &OsStr) -> Vec<u16> {
    OsString::from_wide(&value.encode_wide().collect::<Vec<u16>>())
        .to_string_lossy()
        .to_uppercase()
        .encode_utf16()
        .collect()
}

fn command_line(binary: &Path, args: &[String]) -> Result<Vec<u16>, DocsightError> {
    let mut line = OsString::new();
    append_argument(&mut line, binary.as_os_str())?;
    for argument in args {
        line.push(OsStr::new(" "));
        append_argument(&mut line, OsStr::new(argument))?;
    }
    let encoded = wide(&line);
    if encoded.iter().rev().skip(1).any(|unit| *unit == 0) {
        return Err(sandbox_failure(
            "an isolated worker argument contains a NUL character",
        ));
    }
    Ok(encoded)
}

fn append_argument(line: &mut OsString, argument: &OsStr) -> Result<(), DocsightError> {
    let text = argument
        .to_str()
        .ok_or_else(|| sandbox_failure("isolated worker arguments must be valid UTF-16 text"))?;
    line.push(OsStr::new("\""));
    let mut backslashes = 0usize;
    for character in text.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                line.push(OsStr::new("\\"));
            }
            '"' => {
                for _ in 0..=backslashes {
                    line.push(OsStr::new("\\"));
                }
                backslashes = 0;
                line.push(OsStr::new("\""));
            }
            _ => {
                backslashes = 0;
                let mut buffer = [0u8; 4];
                line.push(OsStr::new(character.encode_utf8(&mut buffer)));
            }
        }
    }
    for _ in 0..backslashes {
        line.push(OsStr::new("\\"));
    }
    line.push(OsStr::new("\""));
    Ok(())
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::from_ne_bytes(unsafe { GetLastError() }.to_ne_bytes()))
}
