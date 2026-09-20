use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn sample_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation/sample_features.docx")
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
        let mut child = docsight()
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
    ];

    for expected in expected_tools {
        assert!(tool_names.contains(&expected), "missing tool: {expected}");
    }

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
