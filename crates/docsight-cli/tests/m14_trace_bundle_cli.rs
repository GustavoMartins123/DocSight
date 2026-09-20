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

#[test]
fn synthetic_pdf_supports_trace_and_proof() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("synthetic.pdf");
    std::fs::copy(fixture("synthetic_table.pdf"), &input)?;
    let trace = temporary.path().join("page.dstrace");
    let render = temporary.path().join("page.png");
    let bundle = temporary.path().join("table.dse");
    let commands = [
        vec![
            "render",
            input.to_str().ok_or("path")?,
            "--page",
            "1",
            "--dpi",
            "36",
            "--out",
            render.to_str().ok_or("path")?,
            "--trace",
            trace.to_str().ok_or("path")?,
        ],
        vec![
            "bundle",
            input.to_str().ok_or("path")?,
            "--object",
            "tbl_b54dbb45da2a78dec493d24c309ea8c0",
            "--dpi",
            "36",
            "--include-crop",
            "--out",
            bundle.to_str().ok_or("path")?,
        ],
    ];
    for arguments in commands {
        let output = docsight().arg("--agent").args(&arguments).output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(output.stderr.is_empty());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["schema"], "docsight.agent/v2");
        assert!(
            json["warnings"]
                .as_array()
                .ok_or("warnings")?
                .iter()
                .any(|warning| warning["code"] == "APPROXIMATED_PDF_FONT")
        );
        assert!(
            json["warnings"]
                .as_array()
                .ok_or("warnings")?
                .iter()
                .all(|warning| warning["code"] != "TRACE_DECISION_PARTIAL")
        );
        if arguments[0] == "render" {
            assert_eq!(json["result"]["trace"]["trace_schema"], "docsight.trace/v2");
            assert_eq!(
                json["result"]["trace"]["decision_coverage"]["display_list_operations"],
                "verified"
            );
            assert_eq!(
                json["result"]["trace"]["decision_coverage"]["pagination"],
                "not_applicable"
            );
            assert!(json["result"]["trace"]["resource_count"].is_u64());
            assert!(json["result"]["trace"]["glyph_run_count"].is_u64());
            assert!(json["result"]["trace"]["display_operation_count"].is_u64());
        }
    }
    std::fs::remove_file(&input)?;
    for arguments in [
        vec!["replay", trace.to_str().ok_or("path")?, "--verify"],
        vec!["verify", bundle.to_str().ok_or("path")?],
    ] {
        let output = docsight().arg("--agent").args(&arguments).output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(output.stderr.is_empty());
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["result"]["verification"]["valid"], true);
        let expected_schema = if arguments[0] == "replay" {
            "docsight.trace/v2"
        } else {
            "docsight.proof-bundle/v2"
        };
        assert_eq!(json["result"]["verification"]["schema"], expected_schema);
        if arguments[0] == "replay" {
            assert_eq!(json["result"]["trace_schema"], "docsight.trace/v2");
            assert_eq!(
                json["result"]["decision_coverage"]["display_list_operations"],
                "verified"
            );
            assert!(json["result"]["resource_count"].is_u64());
            assert!(json["result"]["glyph_run_count"].is_u64());
            assert!(json["result"]["display_operation_count"].is_u64());
        }
    }
    Ok(())
}

#[test]
fn bundle_defaults_to_page_one_and_accepts_page_only() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let document = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/validation/sample_headings.docx");
    let default_bundle = directory.path().join("default.dse");
    let page_bundle = directory.path().join("page1.dse");

    let output = docsight()
        .args(["--agent", "bundle"])
        .arg(&document)
        .args(["--out"])
        .arg(&default_bundle)
        .output()?;
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(json["result"]["target"]["selector"]["kind"], "page");
    assert_eq!(json["result"]["target"]["page"], 1);

    let verify_output = docsight()
        .args(["--agent", "verify"])
        .arg(&default_bundle)
        .output()?;
    assert!(verify_output.status.success());
    let verify_json: serde_json::Value = serde_json::from_slice(&verify_output.stdout)?;
    assert_eq!(verify_json["result"]["verification"]["valid"], true);

    let output_page = docsight()
        .args(["--agent", "bundle"])
        .arg(&document)
        .args(["--page", "1", "--out"])
        .arg(&page_bundle)
        .output()?;
    assert!(output_page.status.success());
    let page_json: serde_json::Value = serde_json::from_slice(&output_page.stdout)?;
    assert_eq!(page_json["result"]["target"]["selector"]["kind"], "page");
    assert_eq!(page_json["result"]["target"]["page"], 1);
    Ok(())
}
