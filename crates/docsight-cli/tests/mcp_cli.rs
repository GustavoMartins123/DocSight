mod support;

use support::encrypted_pdf;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn sample_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_features.docx")
}

fn tables_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_tables.docx")
}

fn headings_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_headings.docx")
}

struct McpClient {
    child: std::process::Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with_args(&[])
    }

    fn start_with_args(args: &[&str]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = docsight()
            .args(args)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("failed to take child stdout")?;
        let reader = BufReader::new(stdout);
        Ok(Self { child, reader })
    }

    fn start_with_password_file(
        path: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = docsight()
            .arg("--password-file")
            .arg(path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("failed to take child stdout")?;
        let reader = BufReader::new(stdout);
        Ok(Self { child, reader })
    }

    fn request(
        &mut self,
        request_json: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let stdin = self.child.stdin.as_mut().ok_or("stdin closed")?;
        stdin.write_all(request_json.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;

        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let response: serde_json::Value = serde_json::from_str(&line)?;
        Ok(response)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_initialize_and_ping() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;

    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let init_res = client.request(&init_req.to_string())?;
    assert_eq!(init_res["jsonrpc"], "2.0");
    assert_eq!(init_res["id"], 1);
    assert_eq!(init_res["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init_res["result"]["serverInfo"]["name"], "docsight");

    let ping_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "ping"
    });
    let ping_res = client.request(&ping_req.to_string())?;
    assert_eq!(ping_res["jsonrpc"], "2.0");
    assert_eq!(ping_res["id"], 2);
    assert!(ping_res["result"].is_object());

    Ok(())
}

#[test]
fn mcp_tools_list_declares_all_tools() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;

    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });
    let list_res = client.request(&list_req.to_string())?;
    assert_eq!(list_res["jsonrpc"], "2.0");
    assert_eq!(list_res["id"], 1);

    let tools = list_res["result"]["tools"]
        .as_array()
        .ok_or("tools not array")?;
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    let expected_tools = [
        "inspect_document",
        "search_document",
        "get_page",
        "get_region",
        "compare_documents",
        "verify_document",
        "get_evidence",
        "replay_bundle",
        "get_overview",
        "get_outline",
        "list_tables",
        "get_table",
        "get_context",
        "resolve_target",
        "get_peek",
        "get_focus",
        "render_crop",
        "create_bundle",
    ];

    for expected in expected_tools {
        assert!(tool_names.contains(&expected), "missing tool: {expected}");
    }
    let inspect = tools
        .iter()
        .find(|tool| tool["name"] == "inspect_document")
        .ok_or("inspect_document missing")?;
    assert!(
        inspect["inputSchema"]["properties"]
            .get("password")
            .is_some()
    );
    let compare = tools
        .iter()
        .find(|tool| tool["name"] == "compare_documents")
        .ok_or("compare_documents missing")?;
    assert!(
        compare["inputSchema"]["properties"]
            .get("password_before_file")
            .is_some()
    );
    assert!(
        compare["inputSchema"]["properties"]
            .get("password_after_file")
            .is_some()
    );

    Ok(())
}

#[test]
fn mcp_tool_inspect_document() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": {
                "path": path_str
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["id"], 1);
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let inspect_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert_eq!(inspect_val["format"], "docx");
    assert_eq!(
        inspect_val["capabilities"]["structure"].as_bool(),
        Some(true)
    );
    assert_eq!(inspect_val["capabilities"]["text"].as_bool(), Some(true));

    Ok(())
}

#[test]
fn mcp_inspect_consumes_and_validates_pdf_passwords() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("encrypted.pdf");
    std::fs::write(&path, encrypted_pdf::build(b"correct horse"))?;
    let path_str = path.to_str().ok_or("invalid path")?;
    let mut client = McpClient::start()?;

    let correct = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": { "path": path_str, "password": "correct horse" }
        }
    });
    let response = client.request(&correct.to_string())?;
    assert_eq!(response["result"]["isError"], false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let inspected: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(inspected["format"], "pdf");

    let wrong = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": { "path": path_str, "password": "wrong secret" }
        }
    });
    let response = client.request(&wrong.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("ENCRYPTED"), "{text}");
    assert!(!text.contains("wrong secret"), "{text}");

    let wrong_type = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": { "path": path_str, "password": 42 }
        }
    });
    let response = client.request(&wrong_type.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("USAGE"), "{text}");

    let password_file = directory.path().join("password.txt");
    std::fs::write(&password_file, b"correct horse")?;
    let mut conflicting = McpClient::start_with_password_file(&password_file)?;
    let conflict = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": { "path": path_str, "password": "correct horse" }
        }
    });
    let response = conflicting.request(&conflict.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("cannot be combined"), "{text}");
    assert!(!text.contains("correct horse"), "{text}");

    Ok(())
}

#[test]
fn mcp_tool_search_document() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "search_document",
            "arguments": {
                "path": path_str,
                "query": "Dados",
                "mode": "literal"
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let search_val: serde_json::Value = serde_json::from_str(content_text)?;
    let matches = search_val["matches"]
        .as_array()
        .ok_or("matches not array")?;
    assert!(matches.len() >= 3);

    Ok(())
}

#[test]
fn mcp_tool_get_page() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 1
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let page_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert_eq!(page_val["number"], 1);
    let width = page_val["width_pt"].as_f64().ok_or("missing width")?;
    let height = page_val["height_pt"].as_f64().ok_or("missing height")?;
    assert!(width > 0.0);
    assert!(height > 0.0);
    let spans = page_val["spans"].as_array().ok_or("spans not array")?;
    assert!(!spans.is_empty());

    Ok(())
}

#[test]
fn mcp_tool_errors_fail_closed_with_structured_error() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;

    let unknown_tool_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    });
    let res = client.request(&unknown_tool_req.to_string())?;
    assert_eq!(res["result"]["isError"], true);
    let text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("USAGE"));

    let missing_arg_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": {}
        }
    });
    let res2 = client.request(&missing_arg_req.to_string())?;
    assert_eq!(res2["result"]["isError"], true);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text2.contains("USAGE"));

    let unknown_method_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "unknown/method"
    });
    let res3 = client.request(&unknown_method_req.to_string())?;
    assert_eq!(res3["error"]["code"], -32601);

    Ok(())
}

#[test]
fn mcp_tool_get_region() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "get_region",
            "arguments": {
                "path": path_str,
                "page": 1,
                "point": "100,100"
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let hit_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert_eq!(hit_val["query_page"], 1);
    assert!(hit_val.get("total_hits").is_some());

    Ok(())
}

#[test]
fn mcp_tool_compare_documents() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "compare_documents",
            "arguments": {
                "before": path_str,
                "after": path_str
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let diff_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert!(diff_val.get("summary").is_some());

    Ok(())
}

#[test]
fn mcp_compare_accepts_distinct_password_files() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let before = directory.path().join("before.pdf");
    let after = directory.path().join("after.pdf");
    let before_password = directory.path().join("before-password.txt");
    let after_password = directory.path().join("after-password.txt");
    std::fs::write(&before, encrypted_pdf::build(b"before-password"))?;
    std::fs::write(&after, encrypted_pdf::build(b"after-password"))?;
    std::fs::write(&before_password, b"before-password")?;
    std::fs::write(&after_password, b"after-password")?;

    let mut client = McpClient::start()?;
    let response = client.request(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 70,
            "method": "tools/call",
            "params": {
                "name": "compare_documents",
                "arguments": {
                    "before": before.to_str().ok_or("invalid before path")?,
                    "after": after.to_str().ok_or("invalid after path")?,
                    "password_before_file": before_password.to_str().ok_or("invalid before password path")?,
                    "password_after_file": after_password.to_str().ok_or("invalid after password path")?
                }
            }
        })
        .to_string(),
    )?;
    assert_eq!(response["result"]["isError"], false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let diff: serde_json::Value = serde_json::from_str(text)?;
    assert!(diff.get("summary").is_some());
    assert!(!response.to_string().contains("before-password"));
    Ok(())
}

#[test]
fn mcp_tool_verify_document() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "verify_document",
            "arguments": {
                "path": path_str
            }
        }
    });

    let res = client.request(&call_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let verify_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert!(verify_val.get("global").is_some());

    Ok(())
}

#[test]
fn mcp_artifact_routing_does_not_depend_on_extensions() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let document = sample_fixture();
    let disguised_document = directory.path().join("document.dse");
    std::fs::copy(&document, &disguised_document)?;
    let disguised_document_str = disguised_document.to_str().ok_or("invalid path")?;

    let mut client = McpClient::start()?;
    let verify_document = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "verify_document",
            "arguments": { "path": disguised_document_str }
        }
    });
    let response = client.request(&verify_document.to_string())?;
    assert_eq!(response["result"]["isError"], false);

    let bundle = directory.path().join("proof.pdf");
    let source = headings_fixture();
    let output = docsight()
        .arg("bundle")
        .arg(source)
        .args(["--page", "1", "--out"])
        .arg(&bundle)
        .arg("--json")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle_str = bundle.to_str().ok_or("invalid path")?;

    let verify_bundle_as_document = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "verify_document",
            "arguments": { "path": bundle_str }
        }
    });
    let response = client.request(&verify_bundle_as_document.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("MALFORMED_DOCUMENT"), "{text}");

    let verify_bundle = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "replay_bundle",
            "arguments": { "bundle_path": bundle_str }
        }
    });
    let response = client.request(&verify_bundle.to_string())?;
    assert_eq!(response["result"]["isError"], false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let verified: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(verified["bundle_name"], "proof.pdf");
    assert_eq!(verified["verification"]["valid"], true);

    let undeclared_path = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "replay_bundle",
            "arguments": { "path": bundle_str }
        }
    });
    let response = client.request(&undeclared_path.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("bundle_path"), "{text}");
    Ok(())
}

#[test]
fn mcp_replay_bundle_returns_the_public_verify_result() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let bundle = directory.path().join("evidence.dse");
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let bundle_str = bundle.to_str().ok_or("invalid bundle path")?;
    let output = docsight()
        .args([
            "--agent", "bundle", path_str, "--page", "1", "--out", bundle_str,
        ])
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let mut client = McpClient::start()?;
    let list = client.request(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 60,
            "method": "tools/list"
        })
        .to_string(),
    )?;
    let replay = list["result"]["tools"]
        .as_array()
        .ok_or("tools not array")?
        .iter()
        .find(|tool| tool["name"] == "replay_bundle")
        .ok_or("replay_bundle missing")?;
    assert_eq!(
        replay["outputSchema"]["$ref"],
        "https://docsight.dev/schemas/v2/verify-result.json"
    );

    let result = client.request(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 61,
            "method": "tools/call",
            "params": {
                "name": "replay_bundle",
                "arguments": { "bundle_path": bundle_str }
            }
        })
        .to_string(),
    )?;
    assert_eq!(result["result"]["isError"], false);
    let text = result["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let verify: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(verify["bundle_name"], "evidence.dse");
    assert_eq!(verify["verification"]["valid"], true);
    Ok(())
}

#[test]
fn mcp_tool_get_evidence() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let page_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 1
            }
        }
    });
    let page_res = client.request(&page_req.to_string())?;
    let page_text = page_res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let page_val: serde_json::Value = serde_json::from_str(page_text)?;
    let obj_id = page_val["spans"][0]["id"]
        .as_str()
        .ok_or("missing span id")?;

    let evidence_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": obj_id
            }
        }
    });

    let res = client.request(&evidence_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["result"]["isError"], false);

    let content_text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let evidence_val: serde_json::Value = serde_json::from_str(content_text)?;
    assert_eq!(evidence_val["object_id"], obj_id);

    Ok(())
}

#[test]
fn mcp_evidence_matches_cli_record_and_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let object = "lnk_863f7882a9faf975dbbdda888b7adccc";

    let output = docsight()
        .args(["--agent", "evidence"])
        .arg(&path)
        .arg(object)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let warnings = cli["warnings"].as_array().ok_or("warnings not array")?;
    assert!(!warnings.is_empty());

    let mut client = McpClient::start()?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": { "path": path_str, "object_id": object }
        }
    });
    let response = client.request(&request.to_string())?;
    assert_eq!(response["result"]["isError"], false);
    let content = response["result"]["content"]
        .as_array()
        .ok_or("content not array")?;
    assert_eq!(content.len(), warnings.len() + 1);
    let text = content[0]["text"].as_str().ok_or("missing text")?;
    let evidence: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(evidence, cli["result"]);
    assert!(evidence["render_fingerprint"].is_null());
    for (index, warning) in warnings.iter().enumerate() {
        let text = content[index + 1]["text"]
            .as_str()
            .ok_or("missing diagnostic text")?;
        let diagnostic: serde_json::Value = serde_json::from_str(text)?;
        assert_eq!(&diagnostic, warning);
    }

    Ok(())
}

#[test]
fn mcp_get_evidence_handles_object_without_geometry_or_unresolvable()
-> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let meta_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "meta"
            }
        }
    });
    let res = client.request(&meta_req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["id"], 20);
    assert_eq!(res["result"]["isError"], true);
    let text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("OBJECT_NOT_FOUND"));

    let doc_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "document"
            }
        }
    });
    let res2 = client.request(&doc_req.to_string())?;
    assert_eq!(res2["jsonrpc"], "2.0");
    assert_eq!(res2["id"], 21);
    assert_eq!(res2["result"]["isError"], true);

    Ok(())
}

#[test]
fn mcp_get_evidence_handles_nonexistent_object() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "nonexistent_obj_99999"
            }
        }
    });
    let res = client.request(&req.to_string())?;
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["id"], 30);
    assert_eq!(res["result"]["isError"], true);
    let text = res["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("OBJECT_NOT_FOUND"));

    Ok(())
}

#[test]
fn mcp_handles_invalid_arguments_across_tools() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let bad_dpi_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 40,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "span_0",
                "dpi": 10
            }
        }
    });
    let res_dpi = client.request(&bad_dpi_req.to_string())?;
    assert_eq!(res_dpi["result"]["isError"], true);
    let text = res_dpi["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("dpi must be between 36 and 600"));

    let bad_page_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 0
            }
        }
    });
    let res_page = client.request(&bad_page_req.to_string())?;
    assert_eq!(res_page["result"]["isError"], true);
    let text = res_page["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("page must be greater than or equal to 1"));

    let bad_bbox_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "get_region",
            "arguments": {
                "path": path_str,
                "page": 1,
                "bbox": "not,a,valid,bbox"
            }
        }
    });
    let res_bbox = client.request(&bad_bbox_req.to_string())?;
    assert_eq!(res_bbox["result"]["isError"], true);

    let malformed_point_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 43,
        "method": "tools/call",
        "params": {
            "name": "get_region",
            "arguments": {
                "path": path_str,
                "page": 1,
                "point": "1"
            }
        }
    });
    let response = client.request(&malformed_point_req.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("point must be formatted as x,y"), "{text}");

    let invalid_coordinate_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 44,
        "method": "tools/call",
        "params": {
            "name": "get_region",
            "arguments": {
                "path": path_str,
                "page": 1,
                "point": "x,2"
            }
        }
    });
    let response = client.request(&invalid_coordinate_req.to_string())?;
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    assert!(text.contains("invalid x coordinate: x"), "{text}");

    let nonexistent_file_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 45,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": {
                "path": "nonexistent_file_does_not_exist.docx"
            }
        }
    });
    let res_file = client.request(&nonexistent_file_req.to_string())?;
    assert_eq!(res_file["result"]["isError"], true);

    Ok(())
}

#[test]
fn mcp_session_survives_tool_errors_in_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "tools/call",
        "params": {
            "name": "inspect_document",
            "arguments": { "path": path_str }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["id"], 50);
    assert_eq!(res1["result"]["isError"], false);

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 51,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "missing_object"
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["id"], 51);
    assert_eq!(res2["result"]["isError"], true);

    let req3 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 52,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 0
            }
        }
    });
    let res3 = client.request(&req3.to_string())?;
    assert_eq!(res3["id"], 52);
    assert_eq!(res3["result"]["isError"], true);

    let req4 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 53,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 1
            }
        }
    });
    let res4 = client.request(&req4.to_string())?;
    assert_eq!(res4["id"], 53);
    assert_eq!(res4["result"]["isError"], false);

    let req5 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 54,
        "method": "tools/call",
        "params": {
            "name": "get_evidence",
            "arguments": {
                "path": path_str,
                "object_id": "any_id",
                "dpi": 5
            }
        }
    });
    let res5 = client.request(&req5.to_string())?;
    assert_eq!(res5["id"], 54);
    assert_eq!(res5["result"]["isError"], true);

    let headings_path = headings_fixture();
    let headings_str = headings_path.to_str().ok_or("invalid path")?;
    let req6 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 55,
        "method": "tools/call",
        "params": {
            "name": "search_document",
            "arguments": {
                "path": headings_str,
                "query": "Dados",
                "mode": "literal"
            }
        }
    });
    let res6 = client.request(&req6.to_string())?;
    assert_eq!(res6["id"], 55);
    assert_eq!(res6["result"]["isError"], false);

    Ok(())
}

#[test]
fn mcp_tool_get_overview_and_outline() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_overview",
            "arguments": {
                "path": path_str,
                "max_items": 2
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);
    let text1 = res1["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val1: serde_json::Value = serde_json::from_str(text1)?;
    assert_eq!(val1["returned_landmarks"], 2);
    assert_eq!(val1["truncated"], true);
    assert!(val1["total_landmarks"].as_u64().unwrap_or(0) > 2);

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_outline",
            "arguments": {
                "path": path_str,
                "max_items": 3
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val2: serde_json::Value = serde_json::from_str(text2)?;
    assert_eq!(val2["returned_headings"], 3);
    assert_eq!(val2["truncated"], true);

    Ok(())
}

#[test]
fn mcp_tool_tables_and_get_table() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = tables_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "list_tables",
            "arguments": {
                "path": path_str,
                "max_items": 2
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);
    let text1 = res1["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val1: serde_json::Value = serde_json::from_str(text1)?;
    assert_eq!(val1["returned_tables"], 2);
    assert_eq!(val1["truncated"], true);
    let first_table_id = val1["tables"][0]["id"].as_str().ok_or("missing table id")?;

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_table",
            "arguments": {
                "path": path_str,
                "object_id": first_table_id,
                "format": "markdown"
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val2: serde_json::Value = serde_json::from_str(text2)?;
    assert_eq!(val2["format"], "markdown");
    assert!(val2["content"].as_str().unwrap_or("").contains('|'));

    let req3 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "get_table",
            "arguments": {
                "path": path_str,
                "object_id": first_table_id,
                "format": "csv"
            }
        }
    });
    let res3 = client.request(&req3.to_string())?;
    assert_eq!(res3["result"]["isError"], false);
    let text3 = res3["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val3: serde_json::Value = serde_json::from_str(text3)?;
    assert_eq!(val3["format"], "csv");
    assert!(val3["content"].as_str().unwrap_or("").contains(','));

    Ok(())
}

#[test]
fn mcp_tool_context_and_resolve() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_context",
            "arguments": {
                "path": path_str,
                "find": "Application Architecture Guide"
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);
    let text1 = res1["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val1: serde_json::Value = serde_json::from_str(text1)?;
    assert!(val1["status"].as_str().is_some());

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "resolve_target",
            "arguments": {
                "path": path_str,
                "text": "Architecture",
                "max_items": 2
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val2: serde_json::Value = serde_json::from_str(text2)?;
    assert_eq!(val2["returned_candidates"], 2);
    assert_eq!(val2["truncated"], true);

    Ok(())
}

#[test]
fn mcp_tool_peek_and_focus() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "get_peek",
            "arguments": {
                "path": path_str,
                "page": 1
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_focus",
            "arguments": {
                "path": path_str,
                "pages": "1..2",
                "max_items": 3
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val2: serde_json::Value = serde_json::from_str(text2)?;
    assert_eq!(val2["returned_objects"], 3);
    assert_eq!(val2["truncated"], true);

    Ok(())
}

#[test]
fn mcp_tool_render_crop_and_create_bundle() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = sample_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;
    let temp_dir = tempfile::tempdir()?;
    let png_path = temp_dir.path().join("crop.png");
    let bundle_path = temp_dir.path().join("bundle.dse");

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "render_crop",
            "arguments": {
                "path": path_str,
                "page": 1,
                "dpi": 72,
                "out": png_path.to_str().ok_or("invalid out path")?
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);
    assert!(png_path.exists());
    assert!(std::fs::metadata(&png_path)?.len() > 0);

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "create_bundle",
            "arguments": {
                "path": path_str,
                "page": 1,
                "out": bundle_path.to_str().ok_or("invalid bundle path")?
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    assert!(bundle_path.exists());

    let req3 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "replay_bundle",
            "arguments": {
                "bundle_path": bundle_path.to_str().ok_or("invalid bundle path")?
            }
        }
    });
    let res3 = client.request(&req3.to_string())?;
    assert_eq!(res3["result"]["isError"], false);
    let text3 = res3["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val3: serde_json::Value = serde_json::from_str(text3)?;
    assert_eq!(val3["verification"]["valid"], true);

    Ok(())
}

#[test]
fn mcp_tool_search_and_page_limits() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = McpClient::start()?;
    let path = headings_fixture();
    let path_str = path.to_str().ok_or("invalid path")?;

    let req1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "search_document",
            "arguments": {
                "path": path_str,
                "query": "de",
                "max_items": 2
            }
        }
    });
    let res1 = client.request(&req1.to_string())?;
    assert_eq!(res1["result"]["isError"], false);
    let text1 = res1["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val1: serde_json::Value = serde_json::from_str(text1)?;
    assert_eq!(val1["returned_matches"], 2);
    assert_eq!(val1["truncated"], true);

    let req2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_page",
            "arguments": {
                "path": path_str,
                "page": 1,
                "max_items": 2
            }
        }
    });
    let res2 = client.request(&req2.to_string())?;
    assert_eq!(res2["result"]["isError"], false);
    let text2 = res2["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing text")?;
    let val2: serde_json::Value = serde_json::from_str(text2)?;
    assert_eq!(val2["spans"].as_array().map(Vec::len), Some(2));
    assert_eq!(val2["spans_truncated"], true);

    Ok(())
}
