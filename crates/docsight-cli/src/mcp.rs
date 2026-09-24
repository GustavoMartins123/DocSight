use crate::cache::DocumentLoader;
use docsight_core::{Diagnostic, DocsightError, compute_coverage};
use docsight_render::HitQuery;
use docsight_search::{FindMode, FindRequest, PageRange, SemanticKind, execute_spatial_query};
use docsight_tables::{table_to_csv, table_to_html, table_to_markdown, table_to_tsv};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

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

struct ToolOutput {
    value: Value,
    diagnostics: Vec<Diagnostic>,
}

impl ToolOutput {
    fn primary(value: Value) -> Self {
        Self {
            value,
            diagnostics: Vec::new(),
        }
    }
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
                    Ok(tool_result) => {
                        let primary = serde_json::to_string_pretty(&tool_result.value)
                            .map_err(serialization_error)?;
                        let mut content = vec![json!({
                            "type": "text",
                            "text": primary
                        })];
                        for diagnostic in &tool_result.diagnostics {
                            content.push(json!({
                                "type": "text",
                                "text": serde_json::to_string_pretty(diagnostic)
                                    .map_err(serialization_error)?
                            }));
                        }
                        JsonRpcResponse {
                            jsonrpc: "2.0",
                            id: request_id,
                            result: Some(json!({
                                "content": content,
                                "isError": false
                            })),
                            error: None,
                        }
                    }
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
                        "description": "Optional non-empty, single-line PDF password of at most 127 bytes. It cannot be combined with an invocation-level password."
                    }
                },
                "required": ["path"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/inspect-result.json"
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
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of matches to return."
                    }
                },
                "required": ["path", "query"]
            },
            "outputSchema": {
                "oneOf": [
                    { "$ref": "https://docsight.dev/schemas/v2/find-result.json" },
                    { "$ref": "https://docsight.dev/schemas/v2/spatial-query-result.json" }
                ]
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
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of text spans to return."
                    }
                },
                "required": ["path", "page"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/page-result.json"
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
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/hit-result.json"
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
                    },
                    "password_before_file": {
                        "type": "string",
                        "description": "Path to a bounded password file for the before document."
                    },
                    "password_after_file": {
                        "type": "string",
                        "description": "Path to a bounded password file for the after document."
                    }
                },
                "required": ["before", "after"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/diff-result.json"
            }
        },
        {
            "name": "verify_document",
            "description": "Verify DOCX or PDF fidelity, glyph coverage, and unsupported feature diagnostics.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the DOCX or PDF document."
                    },
                    "page": {
                        "type": "integer",
                        "description": "Optional page number for page-scoped fidelity report."
                    }
                },
                "required": ["path"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/coverage-report.json"
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
                        "description": "Render resolution DPI for visual hash (default 144)."
                    }
                },
                "required": ["path", "object_id"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/evidence-record.json"
            }
        },
        {
            "name": "replay_bundle",
            "description": "Cryptographically verify a DocSight proof bundle archive independent of its filename extension.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bundle_path": {
                        "type": "string",
                        "description": "Path to the DocSight proof bundle archive."
                    }
                },
                "required": ["bundle_path"]
            },
            "outputSchema": {
                "$ref": "https://docsight.dev/schemas/v2/verify-result.json"
            }
        },
        {
            "name": "get_overview",
            "description": "Retrieve document overview landmarks including headings, tables, and figures in reading order.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of landmarks to return."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "get_outline",
            "description": "Retrieve headings and section hierarchy in reading order.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of headings to return."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "list_tables",
            "description": "List structural or inferred tables in the document with column, row, page, and confidence metrics.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of tables to return."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "get_table",
            "description": "Export or retrieve a specific table by object ID in markdown, csv, tsv, html, or json format.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Semantic object identifier of the table (e.g. 'tbl_...')."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "csv", "tsv", "html", "json"],
                        "description": "Output format (default 'markdown')."
                    }
                },
                "required": ["path", "object_id"]
            }
        },
        {
            "name": "get_context",
            "description": "Aggregate full evidence for an object target or descriptor lookup in one round-trip, including containers, neighborhood, geometry, fidelity, and provenance.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Target semantic object identifier."
                    },
                    "find": {
                        "type": "string",
                        "description": "Text pattern to resolve as context target."
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional object kind constraint for find lookup (e.g. 'table', 'heading')."
                    },
                    "include": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Evidence classes to include: 'content', 'neighbors', 'geometry', 'fidelity', 'provenance', 'heading', 'related'."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "resolve_target",
            "description": "Rank deterministic navigation candidates for a descriptor with explainable component scores.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "text": {
                        "type": "string",
                        "description": "Query text to search and rank."
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional object kind constraint."
                    },
                    "pages": {
                        "type": "string",
                        "description": "Optional page range, e.g. '1..5'."
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of candidates to return."
                    }
                },
                "required": ["path", "text"]
            }
        },
        {
            "name": "get_peek",
            "description": "Retrieve compact structural projection for a page, page range, object, or section.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "page": {
                        "type": "integer",
                        "description": "Single 1-based page number."
                    },
                    "pages": {
                        "type": "string",
                        "description": "Page range, e.g. '1..3'."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Target object identifier."
                    },
                    "section": {
                        "type": "integer",
                        "description": "1-based section index (DOCX only)."
                    },
                    "related": {
                        "type": "boolean",
                        "description": "Include related objects."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "get_focus",
            "description": "Retrieve bounded semantic neighborhood around an object or page range.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Target semantic object identifier."
                    },
                    "pages": {
                        "type": "string",
                        "description": "Page range, e.g. '1..3'."
                    },
                    "related": {
                        "type": "boolean",
                        "description": "Include related objects."
                    },
                    "max_items": {
                        "type": "integer",
                        "description": "Optional maximum number of objects to return."
                    }
                },
                "required": ["path"]
            }
        },
        {
            "name": "render_crop",
            "description": "Render and crop a document page or semantic object to a PNG file on disk with cryptographic digest and dimensions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "out": {
                        "type": "string",
                        "description": "Output path for the generated PNG file."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Target object identifier to crop."
                    },
                    "page": {
                        "type": "integer",
                        "description": "1-based page number to crop."
                    },
                    "bbox": {
                        "type": "string",
                        "description": "Bounding box formatted as 'x0,y0,x1,y1'."
                    },
                    "dpi": {
                        "type": "integer",
                        "description": "Render resolution DPI (default 144, range 36..600)."
                    }
                },
                "required": ["path", "out"]
            }
        },
        {
            "name": "create_bundle",
            "description": "Create a self-contained, verifiable proof bundle archive (.dse) for an object or region.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document."
                    },
                    "out": {
                        "type": "string",
                        "description": "Output path for the .dse bundle file."
                    },
                    "object_id": {
                        "type": "string",
                        "description": "Target object identifier."
                    },
                    "page": {
                        "type": "integer",
                        "description": "1-based page number."
                    },
                    "bbox": {
                        "type": "string",
                        "description": "Bounding box formatted as 'x0,y0,x1,y1'."
                    },
                    "include_crop": {
                        "type": "boolean",
                        "description": "Whether to embed rendered crop PNG in the bundle (default true)."
                    },
                    "dpi": {
                        "type": "integer",
                        "description": "Render DPI for the embedded crop (default 144)."
                    }
                },
                "required": ["path", "out"]
            }
        }
    ])
}

fn execute_tool(
    name: &str,
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<ToolOutput, DocsightError> {
    match name {
        "inspect_document" => Ok(ToolOutput::primary(tool_inspect(arguments, loader)?)),
        "search_document" => Ok(ToolOutput::primary(tool_search(arguments, loader)?)),
        "get_page" => Ok(ToolOutput::primary(tool_get_page(arguments, loader)?)),
        "get_region" => Ok(ToolOutput::primary(tool_get_region(arguments, loader)?)),
        "compare_documents" => Ok(ToolOutput::primary(tool_compare(arguments, loader)?)),
        "verify_document" => Ok(ToolOutput::primary(tool_verify(arguments, loader)?)),
        "get_evidence" => tool_evidence(arguments, loader),
        "replay_bundle" => Ok(ToolOutput::primary(tool_replay_bundle(arguments, loader)?)),
        "get_overview" => Ok(ToolOutput::primary(tool_overview(arguments, loader)?)),
        "get_outline" => Ok(ToolOutput::primary(tool_outline(arguments, loader)?)),
        "list_tables" => Ok(ToolOutput::primary(tool_list_tables(arguments, loader)?)),
        "get_table" => Ok(ToolOutput::primary(tool_get_table(arguments, loader)?)),
        "get_context" => tool_context(arguments, loader),
        "resolve_target" => Ok(ToolOutput::primary(tool_resolve(arguments, loader)?)),
        "get_peek" => Ok(ToolOutput::primary(tool_peek(arguments, loader)?)),
        "get_focus" => Ok(ToolOutput::primary(tool_focus(arguments, loader)?)),
        "render_crop" => tool_render_crop(arguments, loader),
        "create_bundle" => Ok(ToolOutput::primary(tool_create_bundle(arguments, loader)?)),
        _ => Err(DocsightError::InvalidArgument {
            message: format!("Unknown tool name: {name}"),
        }),
    }
}

fn tool_inspect(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let request_password = match arguments.get("password") {
        None => None,
        Some(Value::String(secret)) => {
            if !loader.password().is_empty() {
                return Err(DocsightError::InvalidArgument {
                    message:
                        "MCP inspect password cannot be combined with an invocation-level password"
                            .to_owned(),
                });
            }
            if loader.has_cache() {
                return Err(DocsightError::InvalidArgument {
                    message: "request-scoped document loading cannot use a cache".to_owned(),
                });
            }
            Some(crate::parse_direct_password(secret)?)
        }
        Some(_) => {
            return Err(DocsightError::InvalidArgument {
                message: "password must be a string".to_owned(),
            });
        }
    };
    let request_loader = request_password.as_ref().map(|password| {
        crate::cache::request_scoped_loader(
            password.as_bytes(),
            loader.reporting(),
            loader.max_document_bytes(),
        )
    });
    let active_loader = match &request_loader {
        Some(request_loader) => request_loader,
        None => loader,
    };
    let path = PathBuf::from(path_str);
    let source = active_loader.open_source(&path)?;
    let (result, _warnings) = crate::inspect_source(&source, active_loader)?;
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
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    if mode_str == "spatial" {
        let execution = execute_spatial_query(&document, query_str)?;
        let mut result = execution.result;
        let total_matches = result.total_matches;
        let mut truncated = false;
        if let Some(limit) = max_items.filter(|&limit| result.matches.len() > limit) {
            result.matches.truncate(limit);
            truncated = true;
        }
        let mut val = serde_json::to_value(&result).map_err(serialization_error)?;
        if let Some(obj) = val.as_object_mut() {
            obj.insert("total_matches".to_owned(), json!(total_matches));
            obj.insert("returned_matches".to_owned(), json!(result.matches.len()));
            obj.insert("truncated".to_owned(), json!(truncated));
        }
        Ok(val)
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
        let mut result = docsight_search::find(&document, &request)?;
        let total_matches = result.total_matches;
        let mut truncated = false;
        if let Some(limit) = max_items.filter(|&limit| result.matches.len() > limit) {
            result.matches.truncate(limit);
            truncated = true;
        }
        let mut val = serde_json::to_value(&result).map_err(serialization_error)?;
        if let Some(obj) = val.as_object_mut() {
            obj.insert("total_matches".to_owned(), json!(total_matches));
            obj.insert("returned_matches".to_owned(), json!(result.matches.len()));
            obj.insert("truncated".to_owned(), json!(truncated));
        }
        Ok(val)
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
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let (mut spans, overlays) = crate::page_spans_and_overlays(&document, page_num)?;
    let target_page = document
        .page(page_num)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {page_num}"),
        })?;
    let total_spans = spans.len();
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| spans.len() > limit) {
        spans.truncate(limit);
        truncated = true;
    }
    let page_fidelity = docsight_core::page_fidelity(&document, Some(page_num));
    let page_result = crate::PageResult {
        number: target_page.number,
        width_pt: target_page.width_pt,
        height_pt: target_page.height_pt,
        spans,
        overlays,
        page_fidelity,
    };
    let mut val = serde_json::to_value(page_result).map_err(serialization_error)?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("total_spans".to_owned(), json!(total_spans));
        obj.insert("spans_truncated".to_owned(), json!(truncated));
    }
    Ok(val)
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
            let (x, y) = crate::parse_point(pt)?;
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
    let before_password = optional_password(arguments, "password_before_file")?;
    let after_password = optional_password(arguments, "password_after_file")?;
    let before_password = before_password
        .as_ref()
        .map(crate::PdfPassword::as_bytes)
        .unwrap_or_else(|| loader.password());
    let after_password = after_password
        .as_ref()
        .map(crate::PdfPassword::as_bytes)
        .unwrap_or_else(|| loader.password());

    let options = docsight_diff::DiffOptions {
        visual: false,
        dpi: 144,
        threshold: 8,
        emit_visual_artifacts: false,
    };
    let diff = docsight_diff::diff_documents_with_passwords(
        &before_source,
        &after_source,
        &options,
        before_password,
        after_password,
    )?;
    serde_json::to_value(diff).map_err(serialization_error)
}

fn tool_verify(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let path = PathBuf::from(path_str);

    let page_num = arguments
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;
    let glyph_coverage = docsight_render::document_glyph_coverage(&document, &source);
    let report = compute_coverage(&document, &source, page_num, true, glyph_coverage)?;
    serde_json::to_value(report).map_err(serialization_error)
}

fn tool_evidence(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<ToolOutput, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let object_id = required_string(arguments, "object_id")?;
    let dpi = match arguments.get("dpi") {
        Some(value) => {
            let dpi = value
                .as_u64()
                .ok_or_else(|| DocsightError::InvalidArgument {
                    message: "dpi must be an integer between 36 and 600".to_owned(),
                })?;
            if !(36..=600).contains(&dpi) {
                return Err(DocsightError::InvalidArgument {
                    message: "dpi must be between 36 and 600".to_owned(),
                });
            }
            dpi as u16
        }
        None => crate::DEFAULT_EVIDENCE_RENDER_DPI,
    };
    let result = crate::object_evidence(&PathBuf::from(path_str), loader, object_id, dpi)?;
    Ok(ToolOutput {
        value: serde_json::to_value(result.record).map_err(serialization_error)?,
        diagnostics: result.warnings,
    })
}

fn tool_replay_bundle(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<Value, DocsightError> {
    let bundle_path_str = arguments
        .get("bundle_path")
        .and_then(Value::as_str)
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "Missing required string argument 'bundle_path'".to_owned(),
        })?;
    let bundle_path = PathBuf::from(bundle_path_str);
    let limits = docsight_render::trace::ArtifactLimits {
        max_document_bytes: loader.max_document_bytes(),
    };
    let bundle = docsight_render::trace::read_proof_bundle_with_limits(&bundle_path, limits)?;
    let verification = docsight_render::trace::verify_proof_bundle_with_password(
        &bundle,
        loader.password(),
        limits,
    )?;
    let result = crate::VerifyResult {
        bundle_name: crate::proof_bundle_name(&bundle_path)?,
        verification,
    };
    serde_json::to_value(result).map_err(serialization_error)
}

fn optional_password(
    arguments: &Value,
    key: &str,
) -> Result<Option<crate::PdfPassword>, DocsightError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let path = value
        .as_str()
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: format!("Argument '{key}' must be a string path"),
        })?;
    crate::read_pdf_password(Path::new(path)).map(Some)
}

fn tool_overview(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let mut overview = docsight_search::overview(&document)?;
    let total_landmarks = overview.landmarks.len();
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| overview.landmarks.len() > limit) {
        overview.landmarks.truncate(limit);
        truncated = true;
    }
    let mut val = serde_json::to_value(&overview).map_err(serialization_error)?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("total_landmarks".to_owned(), json!(total_landmarks));
        obj.insert(
            "returned_landmarks".to_owned(),
            json!(overview.landmarks.len()),
        );
        obj.insert("truncated".to_owned(), json!(truncated));
    }
    Ok(val)
}

fn tool_outline(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let mut headings = crate::document_outline(&document);
    let total_headings = headings.len();
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| headings.len() > limit) {
        headings.truncate(limit);
        truncated = true;
    }
    Ok(json!({
        "total_headings": total_headings,
        "returned_headings": headings.len(),
        "truncated": truncated,
        "headings": headings
    }))
}

fn tool_list_tables(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let tables_res = crate::document_tables(&document, &source);
    let total_tables = tables_res.tables.len();
    let mut tables = tables_res.tables;
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| tables.len() > limit) {
        tables.truncate(limit);
        truncated = true;
    }
    Ok(json!({
        "total_tables": total_tables,
        "returned_tables": tables.len(),
        "truncated": truncated,
        "page_fidelity": tables_res.page_fidelity,
        "tables": tables
    }))
}

fn tool_get_table(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let object_id = required_string(arguments, "object_id")?;
    let format_str = arguments
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("markdown");

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let target = document
        .tables()
        .find(|(block, _)| block.id.to_string() == object_id)
        .map(|(_, table)| table)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: object_id.to_owned(),
        })?;

    match format_str {
        "json" => serde_json::to_value(target).map_err(serialization_error),
        "markdown" => {
            let md = table_to_markdown(target)?;
            Ok(json!({
                "format": "markdown",
                "object_id": object_id,
                "rows": target.rows,
                "columns": target.columns,
                "content": md
            }))
        }
        "csv" => {
            let csv = table_to_csv(target)?;
            Ok(json!({
                "format": "csv",
                "object_id": object_id,
                "rows": target.rows,
                "columns": target.columns,
                "content": csv
            }))
        }
        "tsv" => {
            let tsv = table_to_tsv(target)?;
            Ok(json!({
                "format": "tsv",
                "object_id": object_id,
                "rows": target.rows,
                "columns": target.columns,
                "content": tsv
            }))
        }
        "html" => {
            let html = table_to_html(target)?;
            Ok(json!({
                "format": "html",
                "object_id": object_id,
                "rows": target.rows,
                "columns": target.columns,
                "content": html
            }))
        }
        _ => Err(DocsightError::InvalidArgument {
            message: format!(
                "unsupported table format '{format_str}'; expected json, markdown, csv, tsv, or html"
            ),
        }),
    }
}

fn tool_context(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<ToolOutput, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let object_id = arguments.get("object_id").and_then(Value::as_str);
    let find_str = arguments.get("find").and_then(Value::as_str);
    let kind = parse_optional_semantic_kind(arguments.get("kind").and_then(Value::as_str))?;
    let includes = parse_context_includes(arguments.get("include"))?;

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let (context_result, warnings) =
        crate::evaluate_context(&document, &source, object_id, find_str, kind, &includes)?;

    Ok(ToolOutput {
        value: serde_json::to_value(&context_result).map_err(serialization_error)?,
        diagnostics: warnings,
    })
}

fn tool_resolve(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let text_str = required_string(arguments, "text")?;
    let kind = parse_optional_semantic_kind(arguments.get("kind").and_then(Value::as_str))?;
    let pages = parse_optional_pages(arguments.get("pages").and_then(Value::as_str))?;
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let mut resolved = docsight_search::resolve(&document, text_str, kind, pages)?;
    let total_candidates = resolved.total_candidates;
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| resolved.candidates.len() > limit) {
        resolved.candidates.truncate(limit);
        truncated = true;
    }
    let mut val = serde_json::to_value(&resolved).map_err(serialization_error)?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("total_candidates".to_owned(), json!(total_candidates));
        obj.insert(
            "returned_candidates".to_owned(),
            json!(resolved.candidates.len()),
        );
        obj.insert("truncated".to_owned(), json!(truncated));
    }
    Ok(val)
}

fn tool_peek(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let page_opt = arguments
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let pages_opt = parse_optional_pages(arguments.get("pages").and_then(Value::as_str))?;
    let object_opt = arguments.get("object_id").and_then(Value::as_str);
    let section_opt = arguments
        .get("section")
        .and_then(Value::as_u64)
        .map(|s| s as u32);
    let related = arguments
        .get("related")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let result = match (page_opt, pages_opt, object_opt, section_opt) {
        (Some(p), None, None, None) => {
            docsight_search::peek_pages(&document, PageRange { start: p, end: p })?
        }
        (None, Some(range), None, None) => docsight_search::peek_pages(&document, range)?,
        (None, None, Some(obj), None) => docsight_search::peek_object(&document, obj, related)?,
        (None, None, None, Some(sec)) => docsight_search::peek_section(&document, sec)?,
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "peek requires exactly one of 'page', 'pages', 'object_id', or 'section'"
                    .to_owned(),
            });
        }
    };
    serde_json::to_value(&result).map_err(serialization_error)
}

fn tool_focus(arguments: &Value, loader: &DocumentLoader<'_>) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let object_opt = arguments.get("object_id").and_then(Value::as_str);
    let pages_opt = parse_optional_pages(arguments.get("pages").and_then(Value::as_str))?;
    let related = arguments
        .get("related")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_items = arguments
        .get("max_items")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let document = loader.load(&source)?;

    let mut viewport = match (object_opt, pages_opt) {
        (Some(obj), None) => docsight_search::focus_object(&document, obj, related)?,
        (None, Some(range)) => docsight_search::focus_pages(&document, range)?,
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "focus requires either 'object_id' or 'pages', but not both".to_owned(),
            });
        }
    };
    let total_objects = viewport.objects.len();
    let mut truncated = false;
    if let Some(limit) = max_items.filter(|&limit| viewport.objects.len() > limit) {
        viewport.objects.truncate(limit);
        truncated = true;
    }
    let mut val = serde_json::to_value(&viewport).map_err(serialization_error)?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("total_objects".to_owned(), json!(total_objects));
        obj.insert("returned_objects".to_owned(), json!(viewport.objects.len()));
        obj.insert("truncated".to_owned(), json!(truncated));
    }
    Ok(val)
}

fn tool_render_crop(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<ToolOutput, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let out_str = required_string(arguments, "out")?;
    let object_id = arguments.get("object_id").and_then(Value::as_str);
    let page = arguments
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let bbox_str = arguments.get("bbox").and_then(Value::as_str);
    let dpi = match arguments.get("dpi") {
        Some(value) => {
            let val = value
                .as_u64()
                .ok_or_else(|| DocsightError::InvalidArgument {
                    message: "dpi must be an integer between 36 and 600".to_owned(),
                })?;
            if !(36..=600).contains(&val) {
                return Err(DocsightError::InvalidArgument {
                    message: "dpi must be between 36 and 600".to_owned(),
                });
            }
            val as u16
        }
        None => 144,
    };

    let target = match (object_id, page, bbox_str) {
        (Some(id), None, None) => docsight_render::RenderTarget::Object { id: id.to_owned() },
        (None, Some(p), Some(bb)) => {
            let rect = crate::parse_bbox(bb)
                .map_err(|msg| DocsightError::InvalidArgument { message: msg })?;
            docsight_render::RenderTarget::Region {
                page: p,
                bbox: rect,
            }
        }
        (None, Some(p), None) => docsight_render::RenderTarget::Page { page: p },
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "render_crop requires either 'object_id', or 'page' with optional 'bbox'"
                    .to_owned(),
            });
        }
    };

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let request = docsight_render::RenderRequest { target, dpi };
    let rendered =
        docsight_render::render_document_with_password(&source, &request, loader.password())?;
    let out_path = PathBuf::from(out_str);
    rendered.write(&out_path)?;

    let val = json!({
        "output_path": out_str,
        "output_sha256": crate::digest_bytes(rendered.png()),
        "output_bytes": rendered.png().len(),
        "width_px": rendered.metadata.width_px,
        "height_px": rendered.metadata.height_px,
        "page": rendered.metadata.page,
        "dpi": rendered.metadata.dpi,
        "bbox": rendered.metadata.bbox
    });

    Ok(ToolOutput {
        value: val,
        diagnostics: rendered.warnings,
    })
}

fn tool_create_bundle(
    arguments: &Value,
    loader: &DocumentLoader<'_>,
) -> Result<Value, DocsightError> {
    let path_str = required_string(arguments, "path")?;
    let out_str = required_string(arguments, "out")?;
    let object_id = arguments.get("object_id").and_then(Value::as_str);
    let page = arguments
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| p as u32);
    let bbox_str = arguments.get("bbox").and_then(Value::as_str);
    let include_crop = arguments
        .get("include_crop")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let dpi = match arguments.get("dpi") {
        Some(value) => {
            let val = value
                .as_u64()
                .ok_or_else(|| DocsightError::InvalidArgument {
                    message: "dpi must be an integer between 36 and 600".to_owned(),
                })?;
            if !(36..=600).contains(&val) {
                return Err(DocsightError::InvalidArgument {
                    message: "dpi must be between 36 and 600".to_owned(),
                });
            }
            val as u16
        }
        None => 144,
    };

    let target = match (object_id, page, bbox_str) {
        (Some(id), None, None) => docsight_render::RenderTarget::Object { id: id.to_owned() },
        (None, Some(p), Some(bb)) => {
            let rect = crate::parse_bbox(bb)
                .map_err(|msg| DocsightError::InvalidArgument { message: msg })?;
            docsight_render::RenderTarget::Region {
                page: p,
                bbox: rect,
            }
        }
        (None, Some(p), None) => docsight_render::RenderTarget::Page { page: p },
        (None, None, None) => docsight_render::RenderTarget::Page { page: 1 },
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "cannot combine object_id with page or bbox for create_bundle".to_owned(),
            });
        }
    };

    let path = PathBuf::from(path_str);
    let source = loader.open_source(&path)?;
    let request = docsight_render::RenderRequest { target, dpi };
    let proof = docsight_render::trace::create_proof_bundle_with_password(
        &source,
        &request,
        include_crop,
        loader.password(),
    )?;
    let out_path = PathBuf::from(out_str);
    let written = proof.write(&out_path)?;

    Ok(json!({
        "output_path": out_str,
        "output_sha256": written.sha256,
        "output_bytes": written.bytes,
        "evidence_count": proof.manifest.evidence.len(),
        "crop_included": proof.manifest.crop.is_some()
    }))
}

fn parse_optional_semantic_kind(raw: Option<&str>) -> Result<Option<SemanticKind>, DocsightError> {
    match raw {
        Some(s) => match s {
            "paragraph" => Ok(Some(SemanticKind::Paragraph)),
            "heading" => Ok(Some(SemanticKind::Heading)),
            "table" => Ok(Some(SemanticKind::Table)),
            "figure" => Ok(Some(SemanticKind::Figure)),
            "list_item" => Ok(Some(SemanticKind::ListItem)),
            "shape" => Ok(Some(SemanticKind::Shape)),
            "note" => Ok(Some(SemanticKind::Note)),
            "header" => Ok(Some(SemanticKind::Header)),
            "footer" => Ok(Some(SemanticKind::Footer)),
            "watermark" => Ok(Some(SemanticKind::Watermark)),
            "comment_marker" => Ok(Some(SemanticKind::CommentMarker)),
            "annotation" => Ok(Some(SemanticKind::Annotation)),
            "table_cell" => Ok(Some(SemanticKind::TableCell)),
            "hyperlink" => Ok(Some(SemanticKind::Hyperlink)),
            _ => Err(DocsightError::InvalidArgument {
                message: format!("unknown kind '{s}'"),
            }),
        },
        None => Ok(None),
    }
}

fn parse_context_includes(
    val: Option<&Value>,
) -> Result<Vec<crate::ContextInclude>, DocsightError> {
    match val {
        Some(Value::Array(arr)) => {
            let mut result = Vec::new();
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| DocsightError::InvalidArgument {
                        message: "include elements must be strings".to_owned(),
                    })?;
                match s {
                    "content" => result.push(crate::ContextInclude::Content),
                    "neighbors" => result.push(crate::ContextInclude::Neighbors),
                    "geometry" => result.push(crate::ContextInclude::Geometry),
                    "fidelity" => result.push(crate::ContextInclude::Fidelity),
                    "provenance" => result.push(crate::ContextInclude::Provenance),
                    "heading" => result.push(crate::ContextInclude::Heading),
                    "related" => result.push(crate::ContextInclude::Related),
                    _ => {
                        return Err(DocsightError::InvalidArgument {
                            message: format!(
                                "unknown include class '{s}'; valid are content, neighbors, geometry, fidelity, provenance, heading, related"
                            ),
                        });
                    }
                }
            }
            Ok(result)
        }
        Some(_) => Err(DocsightError::InvalidArgument {
            message: "include must be an array of strings".to_owned(),
        }),
        None => Ok(vec![
            crate::ContextInclude::Content,
            crate::ContextInclude::Neighbors,
            crate::ContextInclude::Geometry,
            crate::ContextInclude::Fidelity,
            crate::ContextInclude::Provenance,
            crate::ContextInclude::Heading,
            crate::ContextInclude::Related,
        ]),
    }
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
