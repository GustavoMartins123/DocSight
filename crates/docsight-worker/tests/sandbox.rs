#[cfg(target_os = "linux")]
use docsight_worker::{SANDBOX_CHILD_ENV, SandboxPolicy, run_in_sandbox_with_env};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

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

#[cfg(target_os = "linux")]
#[test]
fn reads_outside_the_allowlist_are_denied() -> Result<(), Box<dyn std::error::Error>> {
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let forbidden_dir = tempfile::tempdir()?;
    let forbidden_path = forbidden_dir.path().join("private.txt");
    std::fs::write(&forbidden_path, b"private")?;
    let output = run_in_sandbox_with_env(
        Some(worker),
        &SandboxPolicy::default(),
        &[
            "--filesystem-probe-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[
            (SANDBOX_CHILD_ENV.to_owned(), "1".to_owned()),
            (
                "DOCSIGHT_FILESYSTEM_PROBE_PATH".to_owned(),
                forbidden_path.to_string_lossy().into_owned(),
            ),
        ],
    )?;
    assert_eq!(output.exit_code, 0);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_budget_terminates_busy_worker() -> Result<(), Box<dyn std::error::Error>> {
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let policy = SandboxPolicy {
        cpu_timeout_secs: 1,
        ..SandboxPolicy::default()
    };
    let start = Instant::now();
    let result = run_in_sandbox_with_env(
        Some(worker),
        &policy,
        &[
            "--cpu-hog-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[(SANDBOX_CHILD_ENV.to_owned(), "1".to_owned())],
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    let error = match result {
        Err(error) => error,
        Ok(_) => {
            return Err(std::io::Error::other("busy worker stayed within its CPU budget").into());
        }
    };
    assert_eq!(error.exit_code(), 30);
    Ok(())
}
