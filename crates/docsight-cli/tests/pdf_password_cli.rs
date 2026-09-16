mod support;

use std::process::Command;
use support::encrypted_pdf;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

#[test]
fn password_file_unlocks_agent_inspection_and_text_deterministically()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let pdf = directory.path().join("protected.pdf");
    let password = directory.path().join("password.txt");
    std::fs::write(&pdf, encrypted_pdf::build(b"test-only-password"))?;
    std::fs::write(&password, b"test-only-password\r\n")?;

    let inspect = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("inspect")
        .arg(&pdf)
        .output()?;
    assert!(inspect.status.success());
    assert!(inspect.stderr.is_empty());
    let repeated = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("inspect")
        .arg(&pdf)
        .output()?;
    assert_eq!(inspect.stdout, repeated.stdout);

    let text = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("text")
        .arg(&pdf)
        .output()?;
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout)?.contains("Confidential"));
    Ok(())
}

#[test]
fn wrong_or_invalid_password_files_fail_closed_without_leaking_the_secret()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let pdf = directory.path().join("protected.pdf");
    let wrong = directory.path().join("wrong.txt");
    let multiline = directory.path().join("multiline.txt");
    std::fs::write(&pdf, encrypted_pdf::build(b"test-only-password"))?;
    std::fs::write(&wrong, b"private-wrong-value")?;
    std::fs::write(&multiline, b"test-only-password\nsecond")?;

    let output = docsight()
        .args(["--agent", "--password-file"])
        .arg(&wrong)
        .arg("inspect")
        .arg(&pdf)
        .output()?;
    assert_eq!(output.status.code(), Some(12));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-wrong-value"));

    let output = docsight()
        .args(["--agent", "--password-file"])
        .arg(&multiline)
        .arg("inspect")
        .arg(&pdf)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "USAGE");
    Ok(())
}

#[test]
fn password_file_is_available_inside_the_sandbox() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let pdf = directory.path().join("protected.pdf");
    let password = directory.path().join("password.txt");
    std::fs::write(&pdf, encrypted_pdf::build(b"test-only-password"))?;
    std::fs::write(&password, b"test-only-password")?;

    let output = docsight()
        .args(["--agent", "--sandbox", "--password-file"])
        .arg(&password)
        .arg("text")
        .arg(&pdf)
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8(output.stdout)?.contains("Confidential"));
    Ok(())
}

#[test]
fn password_flows_through_render_trace_bundle_verification_and_diff()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let pdf = directory.path().join("protected.pdf");
    let password = directory.path().join("password.txt");
    let png = directory.path().join("page.png");
    let trace = directory.path().join("page.dstrace");
    let bundle = directory.path().join("page.dse");
    std::fs::write(&pdf, encrypted_pdf::build(b"test-only-password"))?;
    std::fs::write(&password, b"test-only-password")?;

    let render = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("render")
        .arg(&pdf)
        .args(["--page", "1", "--out"])
        .arg(&png)
        .arg("--trace")
        .arg(&trace)
        .output()?;
    assert!(render.status.success());
    assert!(png.is_file());
    assert!(trace.is_file());

    let replay = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("replay")
        .arg(&trace)
        .arg("--verify")
        .output()?;
    assert!(replay.status.success());

    let create_bundle = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("bundle")
        .arg(&pdf)
        .args(["--page", "1", "--bbox", "0,0,200,100", "--out"])
        .arg(&bundle)
        .arg("--include-crop")
        .output()?;
    assert!(create_bundle.status.success());
    assert!(bundle.is_file());

    let verify = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("verify")
        .arg(&bundle)
        .output()?;
    assert!(verify.status.success());

    let diff = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("diff")
        .arg(&pdf)
        .arg(&pdf)
        .output()?;
    assert!(diff.status.success());
    let diff: serde_json::Value = serde_json::from_slice(&diff.stdout)?;
    assert_eq!(diff["result"]["summary"]["semantic_changes"], 0);
    Ok(())
}

#[test]
fn password_file_is_rejected_for_docx_and_non_decrypting_commands()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let password = directory.path().join("password.txt");
    std::fs::write(&password, b"test-only-password")?;
    let docx = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/validation/sample_headings.docx");

    let inspect = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("inspect")
        .arg(&docx)
        .output()?;
    assert_eq!(inspect.status.code(), Some(2));
    assert!(inspect.stdout.is_empty());

    let fingerprint = docsight()
        .args(["--agent", "--password-file"])
        .arg(&password)
        .arg("fingerprint")
        .arg(&docx)
        .output()?;
    assert_eq!(fingerprint.status.code(), Some(2));
    assert!(fingerprint.stdout.is_empty());
    Ok(())
}
