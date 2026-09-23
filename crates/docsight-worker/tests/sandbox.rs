use docsight_worker::{
    SANDBOX_CHILD_ENV, SANDBOX_READ_PATHS_ENV, SANDBOX_WRITE_PATHS_ENV, SandboxPolicy,
    run_in_sandbox_with_env,
};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn network_access_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?.to_string();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            drop(connection);
        }
    });
    let output = run_in_sandbox_with_env(
        Some(worker),
        &SandboxPolicy::default(),
        &[
            "--network-probe-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[
            (SANDBOX_CHILD_ENV.to_owned(), "1".to_owned()),
            ("DOCSIGHT_NETWORK_PROBE_ENDPOINT".to_owned(), endpoint),
        ],
    )?;
    assert_eq!(
        output.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

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
    assert!(start.elapsed() < Duration::from_secs(15));
    let error = match result {
        Err(error) => error,
        Ok(_) => {
            return Err(std::io::Error::other("busy worker stayed within its CPU budget").into());
        }
    };
    assert_eq!(error.exit_code(), 30);
    #[cfg(unix)]
    assert!(
        error.to_string().contains("CPU time limit of 1 seconds"),
        "{error}"
    );
    Ok(())
}

fn sleeping_worker(
    policy: &SandboxPolicy,
    milliseconds: u64,
) -> Result<docsight_worker::WorkerOutput, docsight_core::DocsightError> {
    run_in_sandbox_with_env(
        Some(Path::new(env!("CARGO_BIN_EXE_docsight-worker"))),
        policy,
        &[
            "--sleep-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[
            (SANDBOX_CHILD_ENV.to_owned(), "1".to_owned()),
            (
                "DOCSIGHT_SLEEP_PROBE_MS".to_owned(),
                milliseconds.to_string(),
            ),
        ],
    )
}

#[test]
fn waiting_does_not_count_against_the_cpu_budget() -> Result<(), Box<dyn std::error::Error>> {
    let policy = SandboxPolicy {
        cpu_timeout_secs: 1,
        wall_timeout_secs: 20,
        ..SandboxPolicy::default()
    };
    let output = sleeping_worker(&policy, 2_500)?;
    assert_eq!(output.exit_code, 0);
    Ok(())
}

#[test]
fn a_blocked_worker_is_stopped_at_the_wall_clock_limit() -> Result<(), Box<dyn std::error::Error>> {
    let policy = SandboxPolicy {
        cpu_timeout_secs: 30,
        wall_timeout_secs: 1,
        ..SandboxPolicy::default()
    };
    let start = Instant::now();
    let error = match sleeping_worker(&policy, 60_000) {
        Err(error) => error,
        Ok(_) => return Err("a worker blocked past its wall-clock limit completed".into()),
    };
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(error.exit_code(), 30);
    assert!(
        error.to_string().contains("wall-clock limit of 1 seconds"),
        "{error}"
    );
    Ok(())
}

#[test]
fn declared_read_paths_stay_readable_inside_the_sandbox() -> Result<(), Box<dyn std::error::Error>>
{
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join("sample_headings.docx")
        .canonicalize()?;
    let output = run_in_sandbox_with_env(
        Some(worker),
        &SandboxPolicy::default(),
        &[
            "--json".to_owned(),
            "inspect".to_owned(),
            fixture.to_string_lossy().into_owned(),
        ],
        &[(SANDBOX_CHILD_ENV.to_owned(), "1".to_owned())],
    )?;
    assert_eq!(
        output.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["format"], "docx");
    Ok(())
}

#[test]
fn declared_write_paths_stay_writable_inside_the_sandbox() -> Result<(), Box<dyn std::error::Error>>
{
    let worker = Path::new(env!("CARGO_BIN_EXE_docsight-worker"));
    let handoff = tempfile::tempdir()?;
    let denied = tempfile::tempdir()?;
    let input = handoff.path().join("probe.input");
    std::fs::write(&input, b"key")?;
    let declared_reads = serde_json::to_string(&[input.to_string_lossy()])?;
    let declared_writes = serde_json::to_string(&[handoff.path().to_string_lossy()])?;

    let output = run_in_sandbox_with_env(
        Some(worker),
        &SandboxPolicy::default(),
        &[
            "--filesystem-write-probe-for-test".to_owned(),
            "inspect".to_owned(),
            "unused".to_owned(),
        ],
        &[
            (SANDBOX_CHILD_ENV.to_owned(), "1".to_owned()),
            (SANDBOX_READ_PATHS_ENV.to_owned(), declared_reads),
            (SANDBOX_WRITE_PATHS_ENV.to_owned(), declared_writes),
            (
                "DOCSIGHT_FILESYSTEM_WRITE_PROBE_DIR".to_owned(),
                handoff.path().to_string_lossy().into_owned(),
            ),
            (
                "DOCSIGHT_FILESYSTEM_WRITE_PROBE_INPUT".to_owned(),
                input.to_string_lossy().into_owned(),
            ),
            (
                "DOCSIGHT_FILESYSTEM_WRITE_PROBE_DENIED".to_owned(),
                denied.path().to_string_lossy().into_owned(),
            ),
        ],
    )?;

    assert_eq!(
        output.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(handoff.path().join("probe.result"))?,
        b"declared write path"
    );
    assert!(!handoff.path().join("probe.partial").exists());
    assert!(!denied.path().join("probe.denied").exists());
    Ok(())
}
