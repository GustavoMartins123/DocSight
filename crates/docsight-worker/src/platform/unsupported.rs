use super::{PipeReader, ProcessExit, SpawnRequest, sandbox_failure};
use crate::{SandboxLimitsReport, SandboxPolicy};
use docsight_core::DocsightError;

pub fn apply_resource_limits(
    _policy: &SandboxPolicy,
) -> Result<SandboxLimitsReport, DocsightError> {
    Err(unsupported())
}

pub fn spawn_isolated(
    _request: &SpawnRequest<'_>,
    _policy: &SandboxPolicy,
) -> Result<IsolatedChild, DocsightError> {
    Err(unsupported())
}

pub struct IsolatedChild {
    never: std::convert::Infallible,
}

impl IsolatedChild {
    pub fn take_stdout(&mut self) -> Option<PipeReader> {
        match self.never {}
    }

    pub fn take_stderr(&mut self) -> Option<PipeReader> {
        match self.never {}
    }

    pub fn try_wait(&mut self) -> Result<Option<ProcessExit>, DocsightError> {
        match self.never {}
    }

    pub fn terminate(&mut self) -> Result<(), DocsightError> {
        match self.never {}
    }
}

fn unsupported() -> DocsightError {
    sandbox_failure("sandbox enforcement is unavailable on this platform")
}
