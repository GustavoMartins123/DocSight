use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::time::Duration;
use xtask::tooling::{common::*, process::*};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
#[ignore = "Invoked as a subprocess fixture by the process tests"]
fn child_process_fixture() -> TestResult { match std::env::var("DOCSIGHT_TOOLING_TEST_MODE")?.as_str() { "output" => { std::io::stdout().write_all(b"out")?; std::io::stderr().write_all(b"err")?; std::process::exit(7); } "timeout" => std::thread::sleep(Duration::from_secs(30)), "overflow" => std::io::stdout().write_all(&vec![b'x'; 1_048_576])?, "stderr_overflow" => std::io::stderr().write_all(&vec![b'x'; 1_048_576])?, "crash" => { #[cfg(windows)] std::process::exit(i32::from_ne_bytes(0xC000_0005u32.to_ne_bytes())); #[cfg(not(windows))] std::process::abort(); }, _ => std::process::exit(99), } Ok(()) }
fn probe(mode: &str, limits: ProcessLimits) -> Result<ProcessResult> { let mut environment = isolated_environment()?; environment.insert("DOCSIGHT_TOOLING_TEST_MODE".into(), mode.into()); let executable = std::env::current_exe()?; let args: Vec<OsString> = vec![executable.into_os_string(), "--exact".into(), "child_process_fixture".into(), "--ignored".into(), "--nocapture".into(), "--test-threads=1".into()]; let directory = tempfile::tempdir()?; run_bounded(&args, &directory.path().canonicalize()?, &limits, Some(&environment)) }
#[test]
fn captures_both_streams_and_nonzero_status() -> TestResult { let result = probe("output", ProcessLimits::default())?; assert_eq!(result.returncode,7); assert!(result.stdout.windows(3).any(|bytes|bytes==b"out")); assert_eq!(result.stderr,b"err"); assert_eq!(result.termination,None); Ok(()) }
#[test]
fn timeout_terminates_the_process_group() -> TestResult { let result=probe("timeout",ProcessLimits{timeout:Duration::from_millis(100),output_bytes:4096})?; assert_eq!(result.termination,Some(Termination::Timeout)); assert!(result.elapsed_ms<10_000); Ok(()) }
#[test]
fn stdout_is_bounded_including_after_exit() -> TestResult { let result=probe("overflow",ProcessLimits{timeout:Duration::from_secs(10),output_bytes:1024})?; assert_eq!(result.termination,Some(Termination::OutputLimit)); assert!(result.stdout.len()+result.stderr.len()<=1024); Ok(()) }
#[test]
fn stderr_shares_the_total_output_budget() -> TestResult { let result=probe("stderr_overflow",ProcessLimits{timeout:Duration::from_secs(10),output_bytes:1024})?; assert_eq!(result.termination,Some(Termination::OutputLimit)); assert!(result.stdout.len()+result.stderr.len()<=1024); Ok(()) }
#[test]
fn abnormal_exit_cannot_look_like_a_typed_error() -> TestResult { let result=probe("crash",ProcessLimits::default())?; assert!(result.returncode<0||result.returncode>65535); assert_eq!(result.termination,None); Ok(()) }
#[test]
fn unavailable_executable_is_a_typed_error() -> TestResult { let directory=tempfile::tempdir()?; let result=run_bounded(&[directory.path().join("definitely-absent")],directory.path(),&ProcessLimits::default(),None); assert_eq!(result.err().map(|error|error.code),Some("EXECUTABLE_UNAVAILABLE")); Ok(()) }
#[test]
fn invalid_process_limits_fail_before_spawn() { for limits in [ProcessLimits{timeout:Duration::ZERO,output_bytes:4},ProcessLimits{timeout:Duration::from_secs(1),output_bytes:0},ProcessLimits{timeout:Duration::from_secs(1),output_bytes:usize::MAX},ProcessLimits{timeout:Duration::MAX,output_bytes:4}] { assert!(limits.validate().is_err()); } assert!(ProcessLimits{timeout:Duration::from_millis(1),output_bytes:1}.validate().is_ok()); }
#[test]
fn isolated_environment_cannot_forward_tokens_or_rust_settings() -> TestResult { let environment:BTreeMap<_,_>=isolated_environment()?; for name in ["GITHUB_TOKEN","AWS_SECRET_ACCESS_KEY","RUSTFLAGS","CARGO_HOME","HOME"] { assert!(!environment.contains_key(&OsString::from(name))); } assert_eq!(environment.get(&OsString::from("NO_COLOR")),Some(&OsString::from("1"))); Ok(()) }
