use crate::WORKER_BACKEND;
use docsight_core::DocsightError;
use std::io::Read;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod posix;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::apply_resource_limits;
#[cfg(target_os = "linux")]
pub use posix::{IsolatedChild, spawn_isolated};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::apply_resource_limits;
#[cfg(target_os = "macos")]
pub use posix::{IsolatedChild, spawn_isolated};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{IsolatedChild, apply_resource_limits, spawn_isolated};

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use unsupported::{IsolatedChild, apply_resource_limits, spawn_isolated};

pub const SANDBOX_BACKEND: &str = "sandbox";

pub struct SpawnRequest<'a> {
    pub binary: &'a Path,
    pub args: &'a [String],
    pub environment: &'a [(String, String)],
    pub read_paths: &'a [String],
    pub write_paths: &'a [String],
    pub temp_dir: Option<&'a Path>,
}

impl SpawnRequest<'_> {
    pub fn child_environment(&self) -> Result<Vec<(String, String)>, DocsightError> {
        let mut environment = self.environment.to_vec();
        environment.push((
            crate::SANDBOX_READ_PATHS_ENV.to_owned(),
            serialize_paths(crate::SANDBOX_READ_PATHS_ENV, self.read_paths)?,
        ));
        environment.push((
            crate::SANDBOX_WRITE_PATHS_ENV.to_owned(),
            serialize_paths(crate::SANDBOX_WRITE_PATHS_ENV, self.write_paths)?,
        ));
        if let Some(temp_dir) = self.temp_dir {
            let temp_dir = temp_dir.to_string_lossy().into_owned();
            for key in ["TMPDIR", "TEMP", "TMP", crate::SANDBOX_TEMP_PATH_ENV] {
                environment.push((key.to_owned(), temp_dir.clone()));
            }
        }
        Ok(environment)
    }
}

fn serialize_paths(variable: &str, paths: &[String]) -> Result<String, DocsightError> {
    serde_json::to_string(paths)
        .map_err(|error| sandbox_failure(format!("failed to serialize {variable}: {error}")))
}

pub enum ProcessExit {
    Code(i64),
    #[cfg(unix)]
    Signal(i32),
}

pub type PipeReader = Box<dyn Read + Send + 'static>;

pub fn sandbox_failure(message: impl Into<String>) -> DocsightError {
    DocsightError::BackendFailure {
        backend: SANDBOX_BACKEND.to_owned(),
        message: message.into(),
    }
}

pub fn worker_failure(message: impl Into<String>) -> DocsightError {
    DocsightError::BackendFailure {
        backend: WORKER_BACKEND.to_owned(),
        message: message.into(),
    }
}
