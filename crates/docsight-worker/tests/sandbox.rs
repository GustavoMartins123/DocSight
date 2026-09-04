#[cfg(target_os = "linux")]
use docsight_worker::{SANDBOX_CHILD_ENV, SandboxPolicy, run_in_sandbox_with_env};
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
#[test]
fn network_syscalls_are_denied() -> Result<(), Box<dyn std::error::Error>> {
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let output = run_in_sandbox_with_env(
        Some(worker),
        &SandboxPolicy::default(),
        &[
            "--network-probe-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[(SANDBOX_CHILD_ENV.to_owned(), "1".to_owned())],
    )?;
    assert_eq!(output.exit_code, 0);
    Ok(())
}
