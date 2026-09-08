use std::path::PathBuf;
use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

#[test]
fn trace_replay_and_proof_verification_are_agent_safe_and_offline()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("input.docx");
    std::fs::copy(fixture("sample_headings.docx"), &input)?;
    let input_str = input.to_str().ok_or("input path")?;
    let render = temporary.path().join("page.png");
    let render_str = render.to_str().ok_or("render path")?;
    let trace = temporary.path().join("page.dstrace");
    let trace_str = trace.to_str().ok_or("trace path")?;
    let bundle = temporary.path().join("heading.dse");
    let bundle_str = bundle.to_str().ok_or("bundle path")?;

    let render_output = docsight()
        .args([
            "--agent", "render", input_str, "--page", "1", "--dpi", "72", "--out", render_str,
            "--trace", trace_str,
        ])
        .output()?;
    assert!(render_output.status.success());
    assert!(render_output.stderr.is_empty());
    let render_json: serde_json::Value = serde_json::from_slice(&render_output.stdout)?;
    assert_eq!(render_json["schema"], "docsight.agent/v2");
    assert!(render_json["result"]["trace"]["output_sha256"].is_string());

    std::fs::remove_file(&input)?;

    let replay_output = docsight()
        .args(["--agent", "replay", trace_str, "--verify"])
        .output()?;
    assert!(replay_output.status.success());
    assert!(replay_output.stderr.is_empty());
    let replay_json: serde_json::Value = serde_json::from_slice(&replay_output.stdout)?;
    assert_eq!(replay_json["result"]["verification"]["valid"], true);

    std::fs::copy(fixture("sample_headings.docx"), &input)?;
    let bundle_output = docsight()
        .args([
            "--agent",
            "bundle",
            input_str,
            "--object",
            "h_515ad605791c12fc496c1c18d79f6526",
            "--dpi",
            "72",
            "--include-crop",
            "--out",
            bundle_str,
        ])
        .output()?;
    assert!(bundle_output.status.success());
    assert!(bundle_output.stderr.is_empty());
    let bundle_json: serde_json::Value = serde_json::from_slice(&bundle_output.stdout)?;
    assert_eq!(bundle_json["result"]["evidence_count"], 1);
    assert_eq!(bundle_json["result"]["crop_included"], true);

    std::fs::remove_file(&input)?;
    let verify_output = docsight()
        .args(["--agent", "verify", bundle_str])
        .output()?;
    assert!(verify_output.status.success());
    assert!(verify_output.stderr.is_empty());
    let verify_json: serde_json::Value = serde_json::from_slice(&verify_output.stdout)?;
    assert_eq!(verify_json["result"]["verification"]["valid"], true);
    assert_eq!(verify_json["result"]["verification"]["crop_verified"], true);

    Ok(())
}

#[test]
fn replay_requires_explicit_verification_mode() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight()
        .args(["replay", "not-present.dstrace"])
        .output()?;

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)?.contains("USAGE"));
    Ok(())
}
