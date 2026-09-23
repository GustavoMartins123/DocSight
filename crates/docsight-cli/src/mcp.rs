use crate::cache::DocumentLoader;
use docsight_core::{DocsightError, ObjectId, compute_coverage, compute_evidence};
use docsight_render::{HitQuery, RenderRequest, RenderTarget, render_document_with_password};
use docsight_search::{FindMode, FindRequest, PageRange, execute_spatial_query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "docsight";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

pub fn run_mcp_server(loader: &DocumentLoader<'_>) -> Result<(), DocsightError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = stdin_lock
            .read_line(&mut line)
            .map_err(|error| DocsightError::Io {
                path: PathBuf::from("<stdin>"),
                source: error,
            })?;
        if bytes_read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(error) => {
                let response = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {error}"),
                        data: None,
                    }),
                };
                serde_json::to_writer(&mut stdout_lock, &response).map_err(serialization_error)?;
                stdout_lock.write_all(b"\n").map_err(io_error)?;
                stdout_lock.flush().map_err(io_error)?;
                continue;
            }
        };

        if request.id.is_none() {
            continue;
        }
        let request_id = request.id.unwrap_or(Value::Null);

        if request.jsonrpc != "2.0" {
            let response = JsonRpcResponse {
                jsonrpc: "2.0",
                id: request_id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid Request: jsonrpc must be '2.0'".to_owned(),
                    data: None,
                }),
            };
            serde_json::to_writer(&mut stdout_lock, &response).map_err(serialization_error)?;
            stdout_lock.write_all(b"\n").map_err(io_error)?;
            stdout_lock.flush().map_err(io_error)?;
            continue;
        }

        let response = match request.method.as_str() {
            "initialize" => JsonRpcResponse {
                jsonrpc: "2.0",
                id: request_id,
                result: Some(json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": SERVER_VERSION
                    }
                })),
                error: None,
            },
            "ping" => JsonRpcResponse {
                jsonrpc: "2.0",
                id: request_id,
                result: Some(json!({})),
                error: None,
            },
            "tools/list" => JsonRpcResponse {
                jsonrpc: "2.0",
                id: request_id,
                result: Some(json!({
                    "tools": list_tools()
                })),
                error: None,
            },
            "tools/call" => {
                let params = request.params.unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                match execute_tool(tool_name, &arguments, loader) {
                    Ok(tool_result) => JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: request_id,
                        result: Some(json!({
                            "content": [
                                {
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&tool_result).unwrap_or_default()
                                }
                            ],
                            "isError": false
                        })),
                        error: None,
                    },
                    Err(tool_error) => {
                        let diagnostic = tool_error.diagnostic();
                        JsonRpcResponse {
                            jsonrpc: "2.0",
                            id: request_id,
                            result: Some(json!({
                                "content": [
                                    {
                                        "type": "text",
                                        "text": format!("{}: {}", diagnostic.code, diagnostic.message)
                                    }
                                ],
                                "isError": true
                            })),
                            error: None,
                        }
                    }
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0",
                id: request_id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                }),
            },
        };

        serde_json::to_writer(&mut stdout_lock, &response).map_err(serialization_error)?;
        stdout_lock.write_all(b"\n").map_err(io_error)?;
        stdout_lock.flush().map_err(io_error)?;
    }

    Ok(())
}

fn list_tools() -> Value {
    json!([
        {
            "name": "inspect_document",
            "description": "Inspect document structure, format, pages, paragraphs, headings, tables, figures, size and fidelity capabilities.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the DOCX or PDF file."
                    },
                    "password": {
                        "type": "string",
                        "description": "Optional password for password-protected PDF documents."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "search_document",
            "description": "Search document content using literal text matching, regular expressions, or spatial DQL queries.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "query": {
                        "type": "string",
                        "description": "Search pattern or spatial query expression."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["literal", "regex", "spatial"],
                        "description": "Search mode: 'literal' for exact text, 'regex' for patterns, 'spatial' for DQL."
                    },
                    "ignore_case": {
                        "type": "boolean",
                        "description": "Case-insensitive matching (default true for literal/regex)."
                    },
                    "pages": {
                        "type": "string",
                        "description": "Optional page range, e.g. '1..5'."
                    }
                },
                "required": ["path", "query"]
            }
        },
        {
            "name": "get_page",
            "description": "Retrieve page geometry dimensions, text spans with bounding boxes, reading order, and fidelity status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "page": {
                        "type": "integer",
                        "description": "1-based page number."
                    }
                },
                "required": ["path", "page"]
            }
        },
        {
            "name": "get_region",
            "description": "Hit test a page region by point (x,y) or bounding box (x0,y0,x1,y1) in PDF points (1/72 in, top-left origin).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "page": {
                        "type": "integer",
                        "description": "1-based page number."
                    },
                    "point": {
                        "type": "string",
                        "description": "Point formatted as 'x,y'."
                    },
                    "bbox": {
                        "type": "string",
                        "description": "Bounding box formatted as 'x0,y0,x1,y1'."
                    }
                },
                "required": ["path", "page"]
            }
        },
        {
            "name": "compare_documents",
            "description": "Compare two documents (DOCX or PDF) producing semantic, structural, and layout diff summaries.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "before": {
                        "type": "string",
                        "description": "Path to the reference/earlier document."
                    },
                    "after": {
                        "type": "string",
                        "description": "Path to the modified/later document."
                    }
                },
                "required": ["before", "after"]
            }
        },
        {
            "name": "verify_document",
            "description": "Verify document fidelity profile, glyph coverage, and unsupported feature diagnostics, or verify a proof bundle.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document or .zip proof bundle."
                    },
                    "page": {
                        "type": "integer",
                        "description": "Optional page number for page-scoped fidelity report."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "get_evidence",
            "description": "Compute cryptographic evidence record for an object including source path, bounding box, render hash and fidelity scores.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Semantic object identifier (e.g. 'p_...', 'tbl_...', 'h_...')."
                    },
                    "dpi": {
                        "type": "integer",
                        "description": "Render resolution DPI for visual hash (default 150)."
                    }
                },
                "required": ["path", "object_id"]
            }
        },
        {
            "name": "replay_bundle",
            "description": "Replay and cryptographically verify a DocSight proof bundle archive (.zip).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bundle_path": {
                        "type": "string",
                        "description": "Path to the .zip proof bundle."
                    }
                },
                "required": ["bundle_path"]
            }
        }
    ])
}

fn execute_tool(
    name: &str,
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<Value, DocsightError> {
    match name {
        "inspect_document" => tool_inspect(arguments, loader),
        "search_document" => tool_search(arguments, loader),
        "get_page" => tool_get_page(arguments, loader),
        "get_region" => tool_get_region(arguments, loader),
        "compare_documents" => tool_compare(arguments, loader),
        "verify_document" => tool_verify(arguments, loader),
        "get_evidence" => tool_evidence(arguments, loader),
        "replay_bundle" => tool_replay_bundle(arguments, loader),
        _ => Err(DocsightError::InvalidArgument {
            message: format!("Unknown tool name: {name}"),
        }),
    }
}

fn tool_inspect(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let (result, _warnings) = crate::inspect_source(&source, loader)?;
    serde_json::to_value(result).map_err(serialization_error)
}

fn tool_search(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let query_str = required_string(arguments, "query")?;
    let mode_str = arguments
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("literal");
    let ignore_case = arguments
        .get("ignore_case")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let pages = parse_optional_pages(arguments.get("pages").and_then(Value::as_str))?;

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    if mode_str == "spatial" {
        let execution = execute_spatial_query(&document, query_str)?;
        serde_json::to_value(execution.result).map_err(serialization_error)
    } else {
        let mode = match mode_str {
            "regex" => FindMode::Regex,
            _ => FindMode::Literal,
        };
        let request = FindRequest {
            pattern: query_str.to_owned(),
            mode,
            ignore_case,
            kinds: BTreeSet::new(),
            pages: pages.map(|p| (p.start, p.end)),
            region: None,
        };
        let result = docsight_search::find(&document, &request)?;
        serde_json::to_value(result).map_err(serialization_error)
    }
}

fn tool_get_page(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let page_num = arguments
        .get("page")
        .and_then(Value::as_u64)
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "Missing required integer argument 'page'".to_owned(),
        })? as u32;
    if page_num == 0 {
        return Err(DocsightError::InvalidArgument {
            message: "page must be greater than or equal to 1".to_owned(),
        });
    }

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let (spans, overlays) = crate::page_spans_and_overlays(&document, page_num)?;
    let target_page = document
        .page(page_num)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {page_num}"),
        })?;
    let page_fidelity = docsight_core::page_fidelity(&document);
    let page_result = crate::PageResult {
        number: target_page.number,
        width_pt: target_page.width_pt,
        height_pt: target_page.height_pt,
        spans,
        overlays,
        page_fidelity,
    };
    serde_json::to_value(page_result).map_err(serialization_error)
}

fn tool_get_region(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let page_num = arguments
        .get("page")
        .and_then(Value::as_u64)
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "Missing required integer argument 'page'".to_owned(),
        })? as u32;
    if page_num == 0 {
        return Err(DocsightError::InvalidArgument {
            message: "page must be greater than or equal to 1".to_owned(),
        });
    }

    let point_str = arguments.get("point").and_then(Value::as_str);
    let bbox_str = arguments.get("bbox").and_then(Value::as_str);

    let query = match (point_str, bbox_str) {
        (Some(pt), None) => {
            let parts: Vec<&str> = pt.split(',').map(str::trim).collect();
            if parts.len() != 2 {
                return Err(DocsightError::InvalidArgument {
                    message: "point must be formatted as 'x,y'".to_owned(),
                });
            }
            let x: f32 = parts[0]
                .parse()
                .map_err(|_| DocsightError::InvalidArgument {
                    message: format!("invalid x: {}", parts[0]),
                })?;
            let y: f32 = parts[1]
                .parse()
                .map_err(|_| DocsightError::InvalidArgument {
                    message: format!("invalid y: {}", parts[1]),
                })?;
            HitQuery::Point(x, y)
        }
        (None, Some(bb)) => {
            let rect = crate::parse_bbox(bb)
                .map_err(|msg| DocsightError::InvalidArgument { message: msg })?;
            HitQuery::BBox(rect)
        }
        (Some(_), Some(_)) => {
            return Err(DocsightError::InvalidArgument {
                message: "cannot provide both 'point' and 'bbox'".to_owned(),
            });
        }
        (None, None) => {
            return Err(DocsightError::InvalidArgument {
                message: "either 'point' or 'bbox' must be provided".to_owned(),
            });
        }
    };

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let result = docsight_render::hit_test(&document, page_num, &query)?;
    serde_json::to_value(result).map_err(serialization_error)
}

fn tool_compare(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let before_str = required_string(arguments, "before")?;
    let after_str = required_string(arguments, "after")?;

    let before_path = PathBuf::from(before_str);
    let after_path = PathBuf::from(after_str);

    let before_source = loader.open_source(&before_path)?;
    let after_source = loader.open_source(&after_path)?;

    let options = docsight_diff::DiffOptions {
        visual: false,
        dpi: 144,
        threshold: 8,
        out_dir: None,
    };
    let diff = docsight_diff::diff_documents_with_passwords(
        &before_source,
        &after_source,
        &options,
        loader.password(),
        loader.password(),
    )?;
    serde_json::to_value(diff).map_err(serialization_error)
}

fn tool_verify(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let path = PathBuf::from(path_str);

    if path_str.ends_with(".zip") || path_str.ends_with(".dse") {
        return tool_replay_bundle(arguments, loader);
    }

    let page_num = arguments
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let glyph_coverage = crate::document_glyph_coverage(&document, &source);
    let report = compute_coverage(&document, &source, page_num, true, glyph_coverage)?;
    serde_json::to_value(report).map_err(serialization_error)
}

fn tool_evidence(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let object_id_str = required_string(arguments, "object_id")?;
    let dpi = match arguments.get("dpi") {
        Some(val) => {
            let n = val.as_u64().ok_or_else(|| DocsightError::InvalidArgument {
                message: "dpi must be an integer between 36 and 600".to_owned(),
            })?;
            if !(36..=600).contains(&n) {
                return Err(DocsightError::InvalidArgument {
                    message: "dpi must be between 36 and 600".to_owned(),
                });
            }
            n as u16
        }
        None => 150,
    };

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let obj_id = ObjectId::from_raw(object_id_str);

    let render_fingerprint = {
        let req = RenderRequest {
            target: RenderTarget::Object {
                id: object_id_str.to_owned(),
            },
            dpi,
        };
        match render_document_with_password(&source, &req, loader.password()) {
            Ok(rendered) => {
                let mut hasher = sha2::Sha256::new();
                use sha2::Digest;
                hasher.update(rendered.png());
                let hash = hasher.finalize();
                Some(
                    hash.iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                )
            }
            Err(_) => None,
        }
    };

    let glyph_coverage = crate::document_glyph_coverage(&document, &source);
    let record = compute_evidence(
        &document,
        &source,
        &obj_id,
        render_fingerprint,
        glyph_coverage,
    )?;
    serde_json::to_value(record).map_err(serialization_error)
}

fn tool_replay_bundle(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<Value, DocsightError> {
    let bundle_path_str = arguments
        .get("bundle_path")
        .or_else(|| arguments.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "Missing required string argument 'bundle_path'".to_owned(),
        })?;
    let bundle_path = PathBuf::from(bundle_path_str);
    let bundle = docsight_render::trace::read_proof_bundle(&bundle_path)?;
    let verification =
        docsight_render::trace::verify_proof_bundle_with_password(&bundle, loader.password())?;
    serde_json::to_value(verification).map_err(serialization_error)
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, DocsightError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: format!("Missing required string argument '{key}'"),
        })
}

fn parse_optional_pages(raw: Option<&str>) -> Result<Option<PageRange>, DocsightError> {
    match raw {
        Some(s) => crate::parse_page_range(s)
            .map(Some)
            .map_err(|msg| DocsightError::InvalidArgument { message: msg }),
        None => Ok(None),
    }
}

fn serialization_error(source: serde_json::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<mcp-response>"),
        source: io::Error::other(source),
    }
}

fn io_error(source: io::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<mcp-stdio>"),
        source,
    }
}
