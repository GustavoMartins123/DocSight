use std::process::Command;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

#[test]
fn completion_scripts_are_available_and_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let output = docsight().args(["completions", shell]).output()?;
        assert!(output.status.success(), "completion failed for {shell}");
        assert!(output.stderr.is_empty());
        assert!(output.stdout.len() > 100, "completion is empty for {shell}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("docsight"),
            "completion does not name docsight for {shell}"
        );
    }

    let first = docsight().args(["completions", "bash"]).output()?;
    let second = docsight().args(["completions", "bash"]).output()?;
    assert_eq!(first.stdout, second.stdout);
    Ok(())
}

#[test]
fn completions_rejects_agent_output_modes() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight()
        .args(["--agent", "completions", "bash"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "USAGE");
    assert_eq!(error["error"]["exit_code"], 2);
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("human setup"))
    );
    Ok(())
}

#[test]
fn capabilities_marks_completions_as_human_only() -> Result<(), Box<dyn std::error::Error>> {
    let output = docsight().args(["--agent", "capabilities"]).output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let capability = value["result"]["commands"]
        .as_array()
        .and_then(|commands| {
            commands
                .iter()
                .find(|command| command["name"] == "completions")
        })
        .ok_or("completions capability")?;
    assert_eq!(capability["ndjson"], false);
    assert_eq!(capability["ndjson_events"], serde_json::json!([]));
    assert_eq!(capability["result_schema"], serde_json::Value::Null);
    assert_eq!(capability["formats"], serde_json::json!([]));
    Ok(())
}
