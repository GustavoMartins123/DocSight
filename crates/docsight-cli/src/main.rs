use clap::{Parser, Subcommand};
use docsight_agent::{
    AgentEnvelope, AgentErrorEnvelope, NdjsonWriter, OutputLimits, QueryLimits,
    apply_bounded_collection, project_json, truncate_json_text_fields, validate_projection,
};
use docsight_core::{
    BlockContent, Diagnostic, DiagnosticSeverity, DocsightError, Document, DocumentFormat,
    DocumentSource, ObjectId, Rect, compute_coverage, compute_evidence, table_to_csv,
    table_to_html, table_to_markdown, table_to_tsv, table_to_tsv_string,
};
use docsight_diff::{DiffOptions, diff_documents};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::{ENGINE_NAME, PdfDocument};
use docsight_render::{HitQuery, RenderRequest, RenderTarget, render_document};
use serde::Serialize;
use sha2::Digest;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "docsight",
    version,
    about = "Headless document inspection for agents"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Use the canonical machine-readable agent contract"
    )]
    agent: bool,

    #[arg(long, global = true, help = "Run the parser in an isolated worker")]
    sandbox: bool,

    #[arg(long, global = true, help = "Emit structured diagnostics on stderr")]
    json_errors: bool,

    #[arg(long, global = true, help = "Emit one JSON object per line")]
    ndjson: bool,

    #[arg(long, global = true, help = "Hard limit for serialized output bytes")]
    max_bytes: Option<usize>,

    #[arg(long, global = true, help = "Maximum number of result items")]
    max_items: Option<usize>,

    #[arg(long, global = true, help = "Maximum characters in text fields")]
    text_limit: Option<usize>,

    #[arg(
        long = "continue",
        global = true,
        help = "Resume from a continuation token"
    )]
    continue_token: Option<String>,

    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        help = "Project selected result fields"
    )]
    select: Option<Vec<String>>,

    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn query_limits(&self) -> QueryLimits {
        QueryLimits {
            max_bytes: self.max_bytes,
            max_items: self.max_items,
            text_limit: self.text_limit,
            continue_token: self.continue_token.clone(),
            select: self.select.clone(),
        }
    }

    fn is_agent_json(&self, subcommand_json: bool) -> bool {
        self.agent
            || subcommand_json
            || self.max_bytes.is_some()
            || self.max_items.is_some()
            || self.text_limit.is_some()
            || self.continue_token.is_some()
            || self.select.is_some()
    }

    fn quiet_mode(&self) -> bool {
        self.quiet || self.agent
    }

    fn structured_errors(&self) -> bool {
        self.json_errors || self.agent
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    Capabilities {
        #[arg(long)]
        json: bool,
    },
    Inspect {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Outline {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Text {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Tables {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Table {
        path: PathBuf,
        object: String,
        #[arg(long, value_enum, default_value_t = TableFormat::Markdown)]
        format: TableFormat,
    },
    Page {
        path: PathBuf,
        page: u32,
        #[arg(long)]
        json: bool,
    },
    Render {
        path: PathBuf,
        #[arg(long)]
        page: u32,
        #[arg(long, default_value_t = 144)]
        dpi: u16,
        #[arg(long)]
        out: PathBuf,
    },
    Crop {
        path: PathBuf,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long, value_parser = parse_bbox)]
        bbox: Option<Rect>,
        #[arg(long)]
        object: Option<String>,
        #[arg(long, default_value_t = 144)]
        dpi: u16,
        #[arg(long)]
        out: PathBuf,
    },
    Images {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Links {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Diff {
        before: PathBuf,
        after: PathBuf,
        #[arg(long)]
        summary: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        visual: bool,
        #[arg(long, default_value_t = 144)]
        dpi: u16,
        #[arg(long, default_value_t = 8)]
        threshold: u8,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    Fingerprint {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Evidence {
        path: PathBuf,
        object: String,
        #[arg(long, default_value_t = 144)]
        render_dpi: u16,
        #[arg(long)]
        json: bool,
    },
    Coverage {
        path: PathBuf,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long)]
        regions: bool,
        #[arg(long)]
        json: bool,
    },
    Hit {
        path: PathBuf,
        #[arg(long)]
        page: u32,
        #[arg(long)]
        point: Option<String>,
        #[arg(long)]
        bbox: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum TableFormat {
    Json,
    Markdown,
    Csv,
    Html,
    Tsv,
}

#[derive(Clone, Debug, Serialize)]
struct InspectCapabilities {
    structure: bool,
    text: bool,
    render: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CapabilityAssessment {
    available: bool,
    source_faithful: bool,
    fidelity: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct InspectCapabilityDetails {
    structure: CapabilityAssessment,
    text: CapabilityAssessment,
    render: CapabilityAssessment,
}

#[derive(Clone, Debug, Serialize)]
struct InspectResult {
    format: DocumentFormat,
    size_bytes: u64,
    capabilities: InspectCapabilities,
    capability_details: InspectCapabilityDetails,
    paragraphs: Option<usize>,
    headings: Option<usize>,
    tables: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    figures: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tracked: Option<docsight_core::TrackedChanges>,
    pages: Option<u32>,
    engine: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
struct HeadingRecord {
    id: ObjectId,
    level: u8,
    text: String,
    source: String,
}

#[derive(Clone, Debug, Serialize)]
struct OutlineResult {
    headings: Vec<HeadingRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct TextRecord {
    id: String,
    kind: &'static str,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
struct TextResult {
    blocks: Vec<TextRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct TableSummary {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    rows: u32,
    columns: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detector: Option<String>,
    review: bool,
    source: String,
}

#[derive(Clone, Debug, Serialize)]
struct TablesResult {
    tables: Vec<TableSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PageSpanRecord {
    id: ObjectId,
    text: String,
    bbox: Rect,
    reading_order: u32,
    confidence: f32,
    source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PageOverlayRecord {
    id: ObjectId,
    kind: docsight_core::OverlayKind,
    text: String,
    bbox: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct PageResult {
    number: u32,
    width_pt: f32,
    height_pt: f32,
    spans: Vec<PageSpanRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    overlays: Vec<PageOverlayRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct ImageRecord {
    id: ObjectId,
    alt_text: Option<String>,
    caption: Option<String>,
    width_pt: Option<f32>,
    height_pt: Option<f32>,
    page: Option<u32>,
    resource: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ImagesResult {
    images: Vec<ImageRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct LinkRecord {
    id: ObjectId,
    text: String,
    target: String,
    is_external: bool,
    page: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
struct LinksResult {
    links: Vec<LinkRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct CommandCapability {
    name: &'static str,
    summary: &'static str,
    formats: &'static [&'static str],
    ndjson: bool,
    bounded: bool,
}

#[derive(Clone, Debug, Serialize)]
struct AgentCapabilitiesResult {
    profile: &'static str,
    protocol: &'static str,
    error_schema: &'static str,
    document_formats: &'static [&'static str],
    output_modes: &'static [&'static str],
    agent_defaults: &'static str,
    limits: &'static [&'static str],
    coordinate_system: &'static str,
    commands: Vec<CommandCapability>,
}

#[derive(Clone, Debug, Serialize)]
struct CapabilitiesEnvelope {
    schema: &'static str,
    engine: &'static str,
    result: AgentCapabilitiesResult,
}

fn main() -> ExitCode {
    let agent_mode = std::env::args().any(|argument| argument == "--agent");
    let sandbox_json_errors =
        agent_mode || std::env::args().any(|argument| argument == "--json-errors");
    if let Err(error) =
        docsight_worker::apply_sandbox_limits_if_child(&docsight_worker::SandboxPolicy::default())
    {
        let exit_code = error.exit_code();
        return if emit_error(&error, sandbox_json_errors, agent_mode).is_ok() {
            ExitCode::from(exit_code)
        } else {
            ExitCode::from(40)
        };
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if agent_mode => {
            let docsight_error = DocsightError::InvalidArgument {
                message: error.to_string(),
            };
            let exit_code = docsight_error.exit_code();
            if emit_error(&docsight_error, true, true).is_err() {
                return ExitCode::from(40);
            }
            return ExitCode::from(exit_code);
        }
        Err(error) => error.exit(),
    };
    if cli.sandbox {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                let error = DocsightError::Io {
                    path: std::env::args()
                        .next()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    source: error,
                };
                let exit_code = error.exit_code();
                return if emit_error(&error, cli.structured_errors(), cli.agent).is_ok() {
                    ExitCode::from(exit_code)
                } else {
                    ExitCode::from(40)
                };
            }
        };
        let raw_args: Vec<String> = std::env::args()
            .skip(1)
            .filter(|arg| arg != "--sandbox")
            .collect();
        match docsight_worker::run_in_sandbox_with_env(
            Some(&exe),
            &docsight_worker::SandboxPolicy::default(),
            &raw_args,
            &[(
                docsight_worker::SANDBOX_CHILD_ENV.to_owned(),
                "1".to_owned(),
            )],
        ) {
            Ok(output) => {
                if io::stdout().write_all(&output.stdout).is_err()
                    || io::stderr().write_all(&output.stderr).is_err()
                {
                    return ExitCode::from(40);
                }
                return ExitCode::from(output.exit_code);
            }
            Err(error) => {
                let exit_code = error.exit_code();
                if emit_error(&error, cli.structured_errors(), cli.agent).is_err() {
                    return ExitCode::from(40);
                }
                return ExitCode::from(exit_code);
            }
        }
    }
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let exit_code = error.exit_code();
            if emit_error(&error, cli.structured_errors(), cli.agent).is_err() {
                return ExitCode::from(40);
            }
            ExitCode::from(exit_code)
        }
    }
}

fn execute(cli: &Cli) -> Result<(), DocsightError> {
    let limits = cli.query_limits();
    let quiet = cli.quiet_mode();
    let json_errors = cli.structured_errors();
    match &cli.command {
        Command::Capabilities { json } => capabilities(cli.is_agent_json(*json), cli.ndjson),
        Command::Inspect { path, json } => inspect(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Outline { path, json } => outline(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Text { path, json } => document_text(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Tables { path, json } => tables(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Table {
            path,
            object,
            format,
        } => table(TableCommandArgs {
            path,
            object,
            format: *format,
            json: cli.is_agent_json(*format == TableFormat::Json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Page { path, page, json } => page_command(
            path,
            *page,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Render {
            path,
            page,
            dpi,
            out,
        } => render(RenderCommandArgs {
            path,
            target: RenderTarget::Page { page: *page },
            dpi: *dpi,
            out,
            command: "render",
            json: cli.is_agent_json(false),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Crop {
            path,
            page,
            bbox,
            object,
            dpi,
            out,
        } => {
            let target = match (page, bbox, object) {
                (Some(page), Some(bbox), None) => RenderTarget::Region {
                    page: *page,
                    bbox: *bbox,
                },
                (None, None, Some(id)) => RenderTarget::Object { id: id.clone() },
                _ => {
                    return Err(DocsightError::InvalidArgument {
                        message: "crop requires either --page with --bbox or only --object"
                            .to_owned(),
                    });
                }
            };
            render(RenderCommandArgs {
                path,
                target,
                dpi: *dpi,
                out,
                command: "crop",
                json: cli.is_agent_json(false),
                ndjson: cli.ndjson,
                limits: &limits,
                quiet,
                json_errors,
            })
        }
        Command::Images { path, json } => images(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Links { path, json } => links(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Diff {
            before,
            after,
            summary,
            json,
            visual,
            dpi,
            threshold,
            out_dir,
        } => diff(DiffCommandArgs {
            before,
            after,
            summary: *summary,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            options: DiffOptions {
                visual: *visual || out_dir.is_some(),
                dpi: *dpi,
                threshold: *threshold,
                out_dir: out_dir.clone(),
            },
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Fingerprint { path, json } => fingerprint(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Evidence {
            path,
            object,
            render_dpi,
            json,
        } => evidence(EvidenceArgs {
            path,
            object,
            render_dpi: *render_dpi,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Coverage {
            path,
            page,
            regions,
            json,
        } => coverage(CoverageArgs {
            path,
            page: *page,
            regions: *regions,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Hit {
            path,
            page,
            point,
            bbox,
            json,
        } => hit(HitArgs {
            path,
            page: *page,
            point: point.as_deref(),
            bbox: bbox.as_deref(),
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
    }
}

fn load_document(source: &DocumentSource) -> Result<Document, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => {
            let unpaginated = parse_docx(source)?;
            let laid_out = layout_docx(unpaginated)?;
            Ok(laid_out.document)
        }
        DocumentFormat::Pdf => {
            let pdf = PdfDocument::open(source)?;
            pdf.to_document()
        }
    }
}

const ALL_DOCUMENT_FORMATS: &[&str] = &["docx", "pdf"];
const DOCX_PDF_FORMATS: &[&str] = &["docx", "pdf"];
const NO_DOCUMENT_FORMATS: &[&str] = &[];
const OUTPUT_MODES: &[&str] = &["json", "ndjson"];
const AGENT_LIMITS: &[&str] = &[
    "--max-bytes",
    "--max-items",
    "--text-limit",
    "--continue",
    "--select",
];

fn capabilities(json: bool, ndjson: bool) -> Result<(), DocsightError> {
    let result = AgentCapabilitiesResult {
        profile: "agent-first-v1",
        protocol: docsight_agent::AGENT_SCHEMA,
        error_schema: "https://docsight.dev/schemas/v2/error-envelope.json",
        document_formats: ALL_DOCUMENT_FORMATS,
        output_modes: OUTPUT_MODES,
        agent_defaults: "JSON on stdout, no diagnostics on stderr, structured errors on stderr",
        limits: AGENT_LIMITS,
        coordinate_system: "points at 1/72 inch with page origin at the top-left",
        commands: vec![
            CommandCapability {
                name: "capabilities",
                summary: "discover the machine contract and command surface",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: true,
                bounded: false,
            },
            CommandCapability {
                name: "inspect",
                summary: "summarize format, counts, capabilities and fidelity",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "outline",
                summary: "return headings in reading order",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "text",
                summary: "return text blocks and deterministic continuation",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "tables",
                summary: "list structural or inferred tables with confidence",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "table",
                summary: "export one table or return its machine record",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "page",
                summary: "return page geometry, spans and overlays",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "images",
                summary: "list figure resources and placements",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "links",
                summary: "list link metadata without fetching targets",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "render",
                summary: "write a PNG artifact with provenance metadata",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "crop",
                summary: "write a page or object crop with provenance metadata",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "diff",
                summary: "compare package, semantic and supported visual changes",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "fingerprint",
                summary: "return reproducibility inputs and result fingerprint",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: false,
            },
            CommandCapability {
                name: "evidence",
                summary: "return provenance and fidelity for one object",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: false,
            },
            CommandCapability {
                name: "coverage",
                summary: "return per-dimension fidelity and reason codes",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
            CommandCapability {
                name: "hit",
                summary: "resolve a point or region to document objects",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                bounded: true,
            },
        ],
    };

    if ndjson {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        let meta = serde_json::json!({
            "seq": 1,
            "type": "meta",
            "schema": docsight_agent::AGENT_SCHEMA,
            "engine": env!("CARGO_PKG_VERSION"),
            "scope": "capabilities",
        });
        let item = serde_json::json!({
            "seq": 2,
            "type": "capabilities",
            "profile": result.profile,
            "protocol": result.protocol,
            "error_schema": result.error_schema,
            "document_formats": result.document_formats,
            "output_modes": result.output_modes,
            "agent_defaults": result.agent_defaults,
            "limits": result.limits,
            "coordinate_system": result.coordinate_system,
            "commands": result.commands,
        });
        let done = serde_json::json!({
            "seq": 3,
            "type": "done",
            "limits": {
                "truncated": false,
                "total_items": 1,
                "returned_items": 1,
            },
        });
        for value in [meta, item, done] {
            serde_json::to_writer(&mut writer, &value).map_err(output_serialization_error)?;
            writer.write_all(b"\n").map_err(stdout_error)?;
        }
        return Ok(());
    }

    if json {
        let value = CapabilitiesEnvelope {
            schema: docsight_agent::AGENT_SCHEMA,
            engine: env!("CARGO_PKG_VERSION"),
            result,
        };
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        serde_json::to_writer(&mut writer, &value).map_err(output_serialization_error)?;
        writer.write_all(b"\n").map_err(stdout_error)?;
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Profile: {}", result.profile).map_err(stdout_error)?;
    writeln!(writer, "Protocol: {}", result.protocol).map_err(stdout_error)?;
    writeln!(writer, "Formats: {}", result.document_formats.join(", ")).map_err(stdout_error)?;
    writeln!(writer, "Output: {}", result.agent_defaults).map_err(stdout_error)?;
    for command in result.commands {
        writeln!(writer, "{}: {}", command.name, command.summary).map_err(stdout_error)?;
    }
    Ok(())
}

fn inspect(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let (result, warnings) = inspect_source(&source)?;

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "inspect".into(),
            source.sha256().to_owned(),
            1,
        )?;
        writer.write_meta(&(&source).into())?;
        let val = serde_json::to_value(&result).map_err(output_serialization_error)?;
        writer.write_item("inspect", &val)?;
        for warning in &warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        return write_single_json(&source, &result, warnings, limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "DOCSIGHT {} | {}",
        env!("CARGO_PKG_VERSION"),
        result.format
    )
    .map_err(stdout_error)?;
    writeln!(writer, "Digest  sha256:{}", source.sha256()).map_err(stdout_error)?;
    writeln!(writer, "Bytes   {}", result.size_bytes).map_err(stdout_error)?;
    if let Some(paragraphs) = result.paragraphs {
        writeln!(writer, "Paragraphs  {paragraphs}").map_err(stdout_error)?;
    }
    if let Some(headings) = result.headings {
        writeln!(writer, "Headings    {headings}").map_err(stdout_error)?;
    }
    if let Some(tables) = result.tables {
        writeln!(writer, "Tables      {tables}").map_err(stdout_error)?;
    }
    if let Some(figures) = result.figures {
        if figures > 0 {
            writeln!(writer, "Figures     {figures}").map_err(stdout_error)?;
        }
    }
    if let Some(comments) = result.comments {
        if comments > 0 {
            writeln!(writer, "Comments    {comments}").map_err(stdout_error)?;
        }
    }
    if let Some(tracked) = result.tracked {
        writeln!(
            writer,
            "Tracked     {} insertions / {} deletions",
            tracked.insertions, tracked.deletions
        )
        .map_err(stdout_error)?;
    }
    if let Some(pages) = result.pages {
        writeln!(writer, "Pages       {pages}").map_err(stdout_error)?;
    }
    emit_warnings(&warnings, quiet, json_errors)
}

fn inspect_source(
    source: &DocumentSource,
) -> Result<(InspectResult, Vec<Diagnostic>), DocsightError> {
    match source.format() {
        DocumentFormat::Docx => {
            let document = layout_docx(parse_docx(source)?)?.document;
            let structure_complete = !document.warnings.iter().any(|warning| {
                warning.code.contains("UNSUPPORTED")
                    && warning.code != "DOCX_RUN_ELEMENT_UNSUPPORTED"
            });
            let text_complete = !document
                .warnings
                .iter()
                .any(|warning| warning.code == "DOCX_RUN_ELEMENT_UNSUPPORTED");
            let render_complete = !document.warnings.iter().any(|warning| {
                matches!(
                    warning.code.as_str(),
                    "DOCX_FONT_SUBSTITUTED"
                        | "DOCX_FIGURE_RASTER_PLACEHOLDER"
                        | "DOCX_BLOCK_TALLER_THAN_PAGE"
                        | "DOCX_PAGINATION_BLOCK_GRANULAR"
                )
            });
            let paragraphs = document.paragraphs().count() + document.list_items().count();
            let tracked = (document.tracked_changes.insertions > 0
                || document.tracked_changes.deletions > 0)
                .then_some(document.tracked_changes);
            let result = InspectResult {
                format: source.format(),
                size_bytes: source.size_bytes(),
                capabilities: InspectCapabilities {
                    structure: structure_complete,
                    text: text_complete,
                    render: render_complete,
                },
                capability_details: InspectCapabilityDetails {
                    structure: CapabilityAssessment {
                        available: true,
                        source_faithful: structure_complete,
                        fidelity: if structure_complete {
                            "exact"
                        } else {
                            "approximated"
                        },
                    },
                    text: CapabilityAssessment {
                        available: true,
                        source_faithful: text_complete,
                        fidelity: if text_complete {
                            "exact"
                        } else {
                            "approximated"
                        },
                    },
                    render: CapabilityAssessment {
                        available: true,
                        source_faithful: render_complete,
                        fidelity: if render_complete {
                            "exact"
                        } else {
                            "approximated"
                        },
                    },
                },
                paragraphs: Some(paragraphs),
                headings: Some(document.headings().count()),
                tables: Some(document.tables().count()),
                figures: Some(document.figures().count()),
                comments: Some(document.comments.len()),
                tracked,
                pages: (!document.pages.is_empty()).then_some(document.pages.len() as u32),
                engine: None,
            };
            Ok((result, document.warnings))
        }
        DocumentFormat::Pdf => inspect_pdf_source(source),
    }
}

fn inspect_pdf_source(
    source: &DocumentSource,
) -> Result<(InspectResult, Vec<Diagnostic>), DocsightError> {
    let unavailable = |pages: Option<u32>, error: DocsightError| {
        let mut diagnostic = error.diagnostic();
        diagnostic.severity = DiagnosticSeverity::Warning;
        let result = InspectResult {
            format: DocumentFormat::Pdf,
            size_bytes: source.size_bytes(),
            capabilities: InspectCapabilities {
                structure: false,
                text: false,
                render: false,
            },
            capability_details: InspectCapabilityDetails {
                structure: CapabilityAssessment {
                    available: false,
                    source_faithful: false,
                    fidelity: "unsupported",
                },
                text: CapabilityAssessment {
                    available: false,
                    source_faithful: false,
                    fidelity: "unsupported",
                },
                render: CapabilityAssessment {
                    available: false,
                    source_faithful: false,
                    fidelity: "unsupported",
                },
            },
            paragraphs: None,
            headings: None,
            tables: None,
            figures: None,
            comments: None,
            tracked: None,
            pages,
            engine: Some(ENGINE_NAME),
        };
        (result, vec![diagnostic])
    };

    let pdf = match PdfDocument::open(source) {
        Ok(pdf) => pdf,
        Err(error @ DocsightError::UnsupportedFeature { .. }) => {
            return Ok(unavailable(None, error));
        }
        Err(error) => return Err(error),
    };
    let pages = Some(pdf.page_count());
    let document = match pdf.to_document() {
        Ok(document) => document,
        Err(error @ DocsightError::UnsupportedFeature { .. }) => {
            return Ok(unavailable(pages, error));
        }
        Err(error) => return Err(error),
    };
    let mut warnings = document.warnings.clone();
    if !warnings
        .iter()
        .any(|warning| warning.code == "INITIAL_PDF_RASTERIZER")
    {
        warnings.push(Diagnostic {
            code: "INITIAL_PDF_RASTERIZER".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: "PDF rendering uses DOCSIGHT's initial native rasterizer".to_owned(),
            effect: "visual output is available with limited antialiasing and glyph fidelity"
                .to_owned(),
            object: None,
            page: None,
        });
    }
    let result = InspectResult {
        format: DocumentFormat::Pdf,
        size_bytes: source.size_bytes(),
        capabilities: InspectCapabilities {
            structure: true,
            text: true,
            render: false,
        },
        capability_details: InspectCapabilityDetails {
            structure: CapabilityAssessment {
                available: true,
                source_faithful: false,
                fidelity: "inferred",
            },
            text: CapabilityAssessment {
                available: true,
                source_faithful: true,
                fidelity: "exact",
            },
            render: CapabilityAssessment {
                available: true,
                source_faithful: false,
                fidelity: "approximated",
            },
        },
        paragraphs: Some(document.paragraphs().count()),
        headings: Some(document.headings().count()),
        tables: Some(document.tables().count()),
        figures: Some(document.figures().count()),
        comments: None,
        tracked: None,
        pages,
        engine: Some(ENGINE_NAME),
    };
    Ok((result, warnings))
}

fn outline(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let headings: Vec<HeadingRecord> = document
        .headings()
        .map(|(block, heading)| HeadingRecord {
            id: block.id.clone(),
            level: heading.level,
            text: heading.text.clone(),
            source: block.source.path.clone(),
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "outline".into(),
            source.sha256().to_owned(),
            headings.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let offset = writer.continuation_offset();
        for heading in headings.iter().skip(offset) {
            let val = serde_json::to_value(heading).map_err(output_serialization_error)?;
            if !writer.write_item("heading", &val)? {
                break;
            }
        }
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        let envelope = apply_bounded_collection(
            &headings,
            limits,
            "outline",
            &source,
            document.warnings,
            |h| {
                serde_json::to_value(OutlineResult { headings: h })
                    .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for heading in &headings {
        let indentation = "  ".repeat(usize::from(heading.level.saturating_sub(1)));
        writeln!(writer, "{indentation}[{}] {}", heading.id, heading.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn page_command(
    path: &PathBuf,
    number: u32,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    if number == 0 {
        return Err(DocsightError::InvalidArgument {
            message: "page numbers are 1-based".to_owned(),
        });
    }
    let target_page = document
        .page(number)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {number}"),
        })?;
    let target_page_number = target_page.number;
    let target_page_width = target_page.width_pt;
    let target_page_height = target_page.height_pt;
    let spans: Vec<PageSpanRecord> = document
        .page_blocks(number)
        .filter_map(|block| {
            let bbox = block.bbox?;
            Some(PageSpanRecord {
                id: block.id.clone(),
                text: block.text(),
                bbox,
                reading_order: block.reading_order,
                confidence: block.confidence,
                source: block.source.path.clone(),
            })
        })
        .collect();
    let overlays: Vec<PageOverlayRecord> = target_page
        .overlays
        .iter()
        .map(|o| PageOverlayRecord {
            id: o.id.clone(),
            kind: o.kind,
            text: o.text.clone(),
            bbox: o.bbox,
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "page".into(),
            source.sha256().to_owned(),
            spans.len() + overlays.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        writer.write_page_begin(number)?;
        let offset = writer.continuation_offset();
        let mut idx = 0;
        for span in &spans {
            if idx >= offset {
                let val = serde_json::to_value(span).map_err(output_serialization_error)?;
                if !writer.write_item("span", &val)? {
                    break;
                }
            }
            idx += 1;
        }
        for overlay in &overlays {
            if idx >= offset {
                let val = serde_json::to_value(overlay).map_err(output_serialization_error)?;
                if !writer.write_item("overlay", &val)? {
                    break;
                }
            }
            idx += 1;
        }
        writer.write_page_end(number)?;
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        #[derive(Clone, Serialize)]
        enum PageItem {
            Span(PageSpanRecord),
            Overlay(PageOverlayRecord),
        }
        let items: Vec<PageItem> = spans
            .iter()
            .map(|span| PageItem::Span(span.clone()))
            .chain(
                overlays
                    .iter()
                    .map(|overlay| PageItem::Overlay(overlay.clone())),
            )
            .collect();
        let envelope = apply_bounded_collection(
            &items,
            limits,
            "page",
            &source,
            document.warnings,
            |bounded| {
                let bounded_spans: Vec<PageSpanRecord> = bounded
                    .iter()
                    .filter_map(|item| match item {
                        PageItem::Span(span) => Some(span.clone()),
                        PageItem::Overlay(_) => None,
                    })
                    .collect();
                let bounded_overlays: Vec<PageOverlayRecord> = bounded
                    .iter()
                    .filter_map(|item| match item {
                        PageItem::Overlay(overlay) => Some(overlay.clone()),
                        PageItem::Span(_) => None,
                    })
                    .collect();
                serde_json::to_value(PageResult {
                    number: target_page_number,
                    width_pt: target_page_width,
                    height_pt: target_page_height,
                    spans: bounded_spans,
                    overlays: bounded_overlays,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Page {}  {}x{} pt  {} spans  {} overlays",
        target_page_number,
        target_page_width,
        target_page_height,
        spans.len(),
        overlays.len()
    )
    .map_err(stdout_error)?;
    for span in &spans {
        writeln!(
            writer,
            "[{}] [{},{},{},{}] {}",
            span.id, span.bbox.x0, span.bbox.y0, span.bbox.x1, span.bbox.y1, span.text
        )
        .map_err(stdout_error)?;
    }
    for overlay in &overlays {
        let bbox_str = overlay
            .bbox
            .map(|b| format!("[{},{},{},{}]", b.x0, b.y0, b.x1, b.y1))
            .unwrap_or_else(|| "none".to_owned());
        writeln!(
            writer,
            "[{}] {:?} {} \"{}\"",
            overlay.id, overlay.kind, bbox_str, overlay.text
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn document_text(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let blocks: Vec<TextRecord> = document
        .blocks
        .iter()
        .map(|block| {
            let (kind, text) = match &block.content {
                BlockContent::Heading(h) => ("heading", h.text.clone()),
                BlockContent::ListItem(li) => ("list_item", li.text.clone()),
                BlockContent::Paragraph(p) => ("paragraph", p.text.clone()),
                BlockContent::Table(t) => ("table", table_to_tsv_string(t)),
                BlockContent::Figure(f) => (
                    "figure",
                    f.caption
                        .clone()
                        .or_else(|| f.alt_text.clone())
                        .unwrap_or_default(),
                ),
                BlockContent::Shape(s) => ("shape", s.label.clone().unwrap_or_default()),
                BlockContent::Note(n) => ("note", n.text.clone()),
                BlockContent::Unknown(u) => ("unknown", u.details.clone().unwrap_or_default()),
            };
            TextRecord {
                id: block.id.to_string(),
                kind,
                text,
            }
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "text".into(),
            source.sha256().to_owned(),
            blocks.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let offset = writer.continuation_offset();
        for block in blocks.iter().skip(offset) {
            let val = serde_json::to_value(block).map_err(output_serialization_error)?;
            if !writer.write_item("block", &val)? {
                break;
            }
        }
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        let envelope =
            apply_bounded_collection(&blocks, limits, "text", &source, document.warnings, |b| {
                serde_json::to_value(TextResult { blocks: b }).map_err(output_serialization_error)
            })?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for block in &blocks {
        writeln!(writer, "[{}] {} {}", block.id, block.kind, block.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn tables(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let tables: Vec<TableSummary> = document
        .tables()
        .map(|(block, table)| {
            let detector = table.detector.clone().or_else(|| {
                (source.format() == DocumentFormat::Pdf).then(|| "inferred".to_owned())
            });
            let review = detector.as_deref().is_some_and(|name| name != "structural")
                && block.confidence < 0.70;
            TableSummary {
                id: block.id.to_string(),
                page: block.page,
                rows: table.rows,
                columns: table.columns,
                confidence: Some(block.confidence),
                detector,
                review,
                source: block.source.path.clone(),
            }
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "tables".into(),
            source.sha256().to_owned(),
            tables.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let offset = writer.continuation_offset();
        for tbl in tables.iter().skip(offset) {
            let val = serde_json::to_value(tbl).map_err(output_serialization_error)?;
            if !writer.write_item("table", &val)? {
                break;
            }
        }
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        let envelope = apply_bounded_collection(
            &tables,
            limits,
            "tables",
            &source,
            document.warnings,
            |tbls| {
                serde_json::to_value(TablesResult { tables: tbls })
                    .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if !tables.is_empty() {
        writeln!(
            writer,
            "{:<12} {:>5}  {:>6}   {:>10}  Detector",
            "ID", "Page", "Size", "Confidence"
        )
        .map_err(stdout_error)?;
        for table in &tables {
            let page_str = table
                .page
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_owned());
            let size_str = format!("{}x{}", table.rows, table.columns);
            let conf_str = table
                .confidence
                .map(|c| format!("{c:.3}"))
                .unwrap_or_else(|| "1.000".to_owned());
            let det_str = table.detector.as_deref().unwrap_or("structural");
            let review_suffix = if table.review { "  [review]" } else { "" };
            writeln!(
                writer,
                "{:<12} {:>5}  {:>6}   {:>10}  {}{}",
                table.id, page_str, size_str, conf_str, det_str, review_suffix
            )
            .map_err(stdout_error)?;
        }
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

struct TableCommandArgs<'a> {
    path: &'a PathBuf,
    object: &'a str,
    format: TableFormat,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn table(args: TableCommandArgs<'_>) -> Result<(), DocsightError> {
    let source = DocumentSource::open(args.path)?;
    let document = load_document(&source)?;
    let target = document
        .tables()
        .find(|(block, _)| block.id.to_string() == args.object)
        .map(|(_, table)| table)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: args.object.to_owned(),
        })?;

    if args.ndjson {
        return write_single_ndjson(
            &source,
            "table",
            "table",
            target,
            &document.warnings,
            args.limits,
        );
    }

    if args.json {
        return write_single_json(&source, target, document.warnings.clone(), args.limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match args.format {
        TableFormat::Json => {
            return write_single_json(&source, target, document.warnings.clone(), args.limits);
        }
        TableFormat::Markdown => {
            let md = table_to_markdown(target)?;
            write!(writer, "{md}").map_err(stdout_error)?;
        }
        TableFormat::Csv => {
            let csv = table_to_csv(target)?;
            write!(writer, "{csv}").map_err(stdout_error)?;
        }
        TableFormat::Html => {
            let html = table_to_html(target)?;
            write!(writer, "{html}").map_err(stdout_error)?;
        }
        TableFormat::Tsv => {
            let tsv = table_to_tsv(target)?;
            write!(writer, "{tsv}").map_err(stdout_error)?;
        }
    }
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

struct RenderCommandArgs<'a> {
    path: &'a PathBuf,
    target: RenderTarget,
    dpi: u16,
    out: &'a Path,
    command: &'a str,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn render(args: RenderCommandArgs<'_>) -> Result<(), DocsightError> {
    let source = DocumentSource::open(args.path)?;
    let rendered = render_document(
        &source,
        &RenderRequest {
            target: args.target,
            dpi: args.dpi,
        },
    )?;
    rendered.write(args.out)?;
    if !args.ndjson && !args.json {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        writeln!(
            writer,
            "Rendered page {} at {} DPI to {} ({}x{} px)",
            rendered.metadata.page,
            rendered.metadata.dpi,
            args.out.display(),
            rendered.metadata.width_px,
            rendered.metadata.height_px
        )
        .map_err(stdout_error)?;
        return emit_warnings(&rendered.warnings, args.quiet, args.json_errors);
    }
    #[derive(Serialize)]
    struct RenderResult {
        page: u32,
        dpi: u16,
        bbox: Rect,
        width_px: u32,
        height_px: u32,
        media_type: &'static str,
        output_path: String,
        output_sha256: String,
        output_bytes: u64,
    }
    let output_path = args
        .out
        .to_str()
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "output path must be valid UTF-8 for agent output".to_owned(),
        })?
        .to_owned();
    let output_sha256 = digest_bytes(rendered.png());
    let output_bytes =
        u64::try_from(rendered.png().len()).map_err(|_| DocsightError::ResourceLimit {
            resource: "rendered artifact bytes".to_owned(),
            limit: u64::MAX,
        })?;
    let result = RenderResult {
        page: rendered.metadata.page,
        dpi: rendered.metadata.dpi,
        bbox: rendered.metadata.bbox,
        width_px: rendered.metadata.width_px,
        height_px: rendered.metadata.height_px,
        media_type: rendered.metadata.media_type,
        output_path,
        output_sha256,
        output_bytes,
    };
    if args.ndjson {
        return write_single_ndjson(
            &source,
            args.command,
            args.command,
            &result,
            &rendered.warnings,
            args.limits,
        );
    }
    write_single_json(&source, &result, rendered.warnings.clone(), args.limits)
}

fn images(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let images: Vec<ImageRecord> = document
        .figures()
        .map(|(block, fig)| ImageRecord {
            id: block.id.clone(),
            alt_text: fig.alt_text.clone(),
            caption: fig.caption.clone(),
            width_pt: fig.width_pt,
            height_pt: fig.height_pt,
            page: block.page,
            resource: fig.resource_id.clone(),
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "images".into(),
            source.sha256().to_owned(),
            images.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let offset = writer.continuation_offset();
        for img in images.iter().skip(offset) {
            let val = serde_json::to_value(img).map_err(output_serialization_error)?;
            if !writer.write_item("image", &val)? {
                break;
            }
        }
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        let envelope = apply_bounded_collection(
            &images,
            limits,
            "images",
            &source,
            document.warnings,
            |imgs| {
                serde_json::to_value(ImagesResult { images: imgs })
                    .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Images: {}", images.len()).map_err(stdout_error)?;
    for img in &images {
        let page_str = img
            .page
            .map(|p| format!("p.{p}"))
            .unwrap_or_else(|| "unplaced".to_owned());
        let dims = match (img.width_pt, img.height_pt) {
            (Some(w), Some(h)) => format!("{w:.1}x{h:.1} pt"),
            _ => "unknown size".to_owned(),
        };
        let label = img
            .alt_text
            .as_deref()
            .or(img.caption.as_deref())
            .unwrap_or("");
        writeln!(writer, "[{}] {} {} \"{}\"", img.id, page_str, dims, label)
            .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn links(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let links: Vec<LinkRecord> = document
        .links
        .iter()
        .map(|link| LinkRecord {
            id: link.id.clone(),
            text: link.text.clone(),
            target: link.target.clone(),
            is_external: link.is_external,
            page: link.page,
        })
        .collect();

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "links".into(),
            source.sha256().to_owned(),
            links.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let offset = writer.continuation_offset();
        for link in links.iter().skip(offset) {
            let val = serde_json::to_value(link).map_err(output_serialization_error)?;
            if !writer.write_item("link", &val)? {
                break;
            }
        }
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if json {
        let envelope = apply_bounded_collection(
            &links,
            limits,
            "links",
            &source,
            document.warnings,
            |lnks| {
                serde_json::to_value(LinksResult { links: lnks })
                    .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Links: {}", links.len()).map_err(stdout_error)?;
    for link in &links {
        let page_str = link
            .page
            .map(|p| format!("p.{p}"))
            .unwrap_or_else(|| "unplaced".to_owned());
        let kind_str = if link.is_external { "EXT" } else { "INT" };
        writeln!(
            writer,
            "[{}] {} [{}] \"{}\" -> {}",
            link.id, page_str, kind_str, link.text, link.target
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn write_single_json<T: Serialize>(
    source: &DocumentSource,
    result: &T,
    warnings: Vec<Diagnostic>,
    limits: &QueryLimits,
) -> Result<(), DocsightError> {
    let mut val = serde_json::to_value(result).map_err(output_serialization_error)?;
    let mut text_truncated = false;
    if let Some(text_limit) = limits.text_limit {
        text_truncated = truncate_json_text_fields(&mut val, text_limit);
    }
    if let Some(ref select) = limits.select {
        validate_projection(&val, select)?;
        val = project_json(&val, select);
    }
    let total_warnings = warnings.len();
    let mut selected_warnings = warnings;
    let mut warnings_truncated = false;
    loop {
        let limits_record = OutputLimits {
            text_truncated,
            warnings_truncated,
            total_warnings: warnings_truncated.then_some(total_warnings),
            returned_warnings: warnings_truncated.then_some(selected_warnings.len()),
            ..OutputLimits::default()
        };
        let envelope = AgentEnvelope::with_limits(
            source,
            val.clone(),
            selected_warnings.clone(),
            limits_record,
        );
        if let Some(max_bytes) = limits.max_bytes {
            let serialized =
                serde_json::to_string(&envelope).map_err(output_serialization_error)?;
            if serialized.len().saturating_add(1) > max_bytes {
                if selected_warnings.pop().is_some() {
                    warnings_truncated = true;
                    continue;
                }
                return Err(DocsightError::InvalidArgument {
                    message: format!(
                        "--max-bytes {max_bytes} is smaller than the requested single-result payload; increase the cap or use --select to reduce the output"
                    ),
                });
            }
        }
        return write_envelope(&envelope);
    }
}

fn write_single_ndjson<T: Serialize>(
    source: &DocumentSource,
    command: &str,
    item_type: &str,
    result: &T,
    warnings: &[Diagnostic],
    limits: &QueryLimits,
) -> Result<(), DocsightError> {
    let stdout = io::stdout();
    let mut writer = NdjsonWriter::new(
        stdout.lock(),
        limits.clone(),
        command.to_owned(),
        source.sha256().to_owned(),
        1,
    )?;
    writer.write_meta(&source.into())?;
    let value = serde_json::to_value(result).map_err(output_serialization_error)?;
    writer.write_item(item_type, &value)?;
    for warning in warnings {
        writer.write_warning(warning)?;
    }
    writer.finish()?;
    Ok(())
}

fn write_envelope<T: Serialize>(envelope: &AgentEnvelope<T>) -> Result<(), DocsightError> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, envelope).map_err(output_serialization_error)?;
    writer.write_all(b"\n").map_err(stdout_error)
}

fn emit_warnings(
    warnings: &[Diagnostic],
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    if quiet {
        return Ok(());
    }
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    for warning in warnings {
        if json_errors {
            let val = serde_json::json!({
                "schema": docsight_agent::AGENT_SCHEMA,
                "warning": warning,
            });
            let line = serde_json::to_string(&val).map_err(output_serialization_error)?;
            writeln!(writer, "{line}").map_err(stderr_error)?;
        } else {
            writeln!(writer, "{}: {}", warning.code, warning.message).map_err(stderr_error)?;
        }
    }
    Ok(())
}

fn emit_error(error: &DocsightError, json: bool, agent: bool) -> io::Result<()> {
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    if agent {
        let envelope = AgentErrorEnvelope::from_error(error);
        serde_json::to_writer(&mut writer, &envelope).map_err(io::Error::other)?;
        writer.write_all(b"\n")
    } else if json {
        let diagnostic = error.diagnostic();
        serde_json::to_writer(&mut writer, &diagnostic).map_err(io::Error::other)?;
        writer.write_all(b"\n")
    } else {
        let diagnostic = error.diagnostic();
        writeln!(writer, "{}: {}", diagnostic.code, diagnostic.message)
    }
}

fn output_serialization_error(source: serde_json::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stdout>"),
        source: io::Error::other(source),
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn stdout_error(source: io::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stdout>"),
        source,
    }
}

fn stderr_error(source: io::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stderr>"),
        source,
    }
}

fn parse_bbox(raw: &str) -> Result<Rect, String> {
    let values = raw
        .split(',')
        .map(str::trim)
        .map(|value| {
            value
                .parse::<f32>()
                .map_err(|error| format!("invalid coordinate '{value}': {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() != 4 {
        return Err(format!(
            "bounding box expects 4 comma-separated numbers, got {}",
            values.len()
        ));
    }
    Rect::new(values[0], values[1], values[2], values[3]).map_err(|error| error.to_string())
}

struct DiffCommandArgs<'a> {
    before: &'a Path,
    after: &'a Path,
    summary: bool,
    json: bool,
    ndjson: bool,
    options: DiffOptions,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn diff(args: DiffCommandArgs<'_>) -> Result<(), DocsightError> {
    let source_before = DocumentSource::open(args.before)?;
    let source_after = DocumentSource::open(args.after)?;
    let diff_result = diff_documents(&source_before, &source_after, &args.options)?;

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            args.limits.clone(),
            "diff".into(),
            source_after.sha256().to_owned(),
            1 + diff_result.semantic.records.len(),
        )?;
        writer.write_meta(&(&source_after).into())?;
        let summary_val =
            serde_json::to_value(&diff_result.summary).map_err(output_serialization_error)?;
        writer.write_item("diff.summary", &summary_val)?;
        for record in &diff_result.semantic.records {
            let record_val = serde_json::to_value(record).map_err(output_serialization_error)?;
            if !writer.write_item("diff.semantic", &record_val)? {
                break;
            }
        }
        for warning in &diff_result.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        return write_single_json(
            &source_after,
            &diff_result,
            diff_result.warnings.clone(),
            args.limits,
        );
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{}", diff_result.summary.format_summary()).map_err(stdout_error)?;
    if !args.summary {
        for record in &diff_result.semantic.records {
            let page_str = record.page.map(|p| format!(" [p.{p}]")).unwrap_or_default();
            writeln!(
                writer,
                "  * {:<8} {}{}: {}",
                record.kind, record.target_type, page_str, record.description
            )
            .map_err(stdout_error)?;
        }
    }
    if let Some(ref dir) = args.options.out_dir {
        writeln!(writer, "Visual diffs written to {}", dir.display()).map_err(stdout_error)?;
    }
    emit_warnings(&diff_result.warnings, args.quiet, args.json_errors)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct FingerprintRecord {
    file_sha256: String,
    engine: String,
    ooxml_engine: String,
    pdf_engine: String,
    raster_engine: String,
    fonts: String,
    layout_profile: String,
    result_fingerprint: String,
}

fn fingerprint(
    path: &Path,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let file_sha256 = source.sha256().to_owned();
    let engine = format!("docsight {}", env!("CARGO_PKG_VERSION"));
    let ooxml_engine = format!("docsight-ooxml {}", env!("CARGO_PKG_VERSION"));
    let pdf_engine = format!("docsight-pdf {}", env!("CARGO_PKG_VERSION"));
    let raster_engine = format!("docsight-render {}", env!("CARGO_PKG_VERSION"));
    let layout_font = docsight_layout::font_fingerprint();
    let raster_font = docsight_render::raster_font_fingerprint();
    let fonts = format!("layout:{layout_font}|raster:{raster_font}");
    let layout_profile = "agent-fidelity-v1".to_owned();

    let mut hasher = sha2::Sha256::new();
    hasher.update(file_sha256.as_bytes());
    hasher.update(b"|");
    hasher.update(engine.as_bytes());
    hasher.update(b"|");
    hasher.update(ooxml_engine.as_bytes());
    hasher.update(b"|");
    hasher.update(pdf_engine.as_bytes());
    hasher.update(b"|");
    hasher.update(raster_engine.as_bytes());
    hasher.update(b"|");
    hasher.update(fonts.as_bytes());
    hasher.update(b"|");
    hasher.update(layout_profile.as_bytes());
    hasher.update(b"|");
    let hash = hasher.finalize();
    let mut result_fingerprint = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(&mut result_fingerprint, "{byte:02x}");
    }

    let record = FingerprintRecord {
        file_sha256,
        engine,
        ooxml_engine,
        pdf_engine,
        raster_engine,
        fonts,
        layout_profile,
        result_fingerprint,
    };

    if ndjson {
        return write_single_ndjson(&source, "fingerprint", "fingerprint", &record, &[], limits);
    }

    if json {
        return write_single_json(&source, &record, Vec::new(), limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{:<18} {}", "file_sha256", record.file_sha256).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "engine", record.engine).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "ooxml_engine", record.ooxml_engine).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "pdf_engine", record.pdf_engine).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "raster_engine", record.raster_engine).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "fonts", record.fonts).map_err(stdout_error)?;
    writeln!(writer, "{:<18} {}", "layout_profile", record.layout_profile).map_err(stdout_error)?;
    writeln!(
        writer,
        "{:<18} {}",
        "result_fingerprint", record.result_fingerprint
    )
    .map_err(stdout_error)?;
    emit_warnings(&[], quiet, json_errors)
}

struct EvidenceArgs<'a> {
    path: &'a Path,
    object: &'a str,
    render_dpi: u16,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn evidence(args: EvidenceArgs<'_>) -> Result<(), DocsightError> {
    let source = DocumentSource::open(args.path)?;
    let doc = load_document(&source)?;
    let obj_id = ObjectId::from_raw(args.object);
    let mut extra_warnings = Vec::new();

    let render_fingerprint = {
        let req = RenderRequest {
            target: RenderTarget::Object {
                id: args.object.to_owned(),
            },
            dpi: args.render_dpi,
        };
        match render_document(&source, &req) {
            Ok(rendered) => {
                let mut hasher = sha2::Sha256::new();
                hasher.update(rendered.png());
                let hash = hasher.finalize();
                let s = hash.iter().map(|byte| format!("{byte:02x}")).collect();
                Some(s)
            }
            Err(render_error) => {
                extra_warnings.push(Diagnostic {
                    code: "RENDER_FINGERPRINT_UNAVAILABLE".to_owned(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "render fingerprint was not computed for {}: {render_error}",
                        args.object
                    ),
                    effect: "visual provenance for this object is missing".to_owned(),
                    object: Some(obj_id.clone()),
                    page: None,
                });
                None
            }
        }
    };

    let glyph_coverage = document_glyph_coverage(&doc, &source);
    let record = compute_evidence(&doc, &source, &obj_id, render_fingerprint, glyph_coverage)?;
    let mut warnings = doc.warnings.clone();
    warnings.extend(extra_warnings);

    if args.ndjson {
        return write_single_ndjson(
            &source,
            "evidence",
            "evidence",
            &record,
            &warnings,
            args.limits,
        );
    }

    if args.json {
        return write_single_json(&source, &record, warnings, args.limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Evidence for {}:", record.object_id).map_err(stdout_error)?;
    writeln!(writer, "  Kind:              {:?}", record.kind).map_err(stdout_error)?;
    writeln!(writer, "  Source Path:       {}", record.source_path).map_err(stdout_error)?;
    if let Some(page) = record.page {
        writeln!(writer, "  Page:              {}", page).map_err(stdout_error)?;
    }
    if let Some(bbox) = record.bbox {
        writeln!(
            writer,
            "  Bounding Box:      [{:.1}, {:.1}, {:.1}, {:.1}]",
            bbox.x0, bbox.y0, bbox.x1, bbox.y1
        )
        .map_err(stdout_error)?;
    }
    if let Some(conf) = record.confidence {
        writeln!(writer, "  Confidence:        {:.3}", conf).map_err(stdout_error)?;
    }
    writeln!(writer, "  Fidelity Profile:").map_err(stdout_error)?;
    writeln!(writer, "    Text:            {:.3}", record.fidelity.text).map_err(stdout_error)?;
    writeln!(
        writer,
        "    Structure:       {:.3}",
        record.fidelity.structure
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "    Geometry:        {:.3}",
        record.fidelity.geometry
    )
    .map_err(stdout_error)?;
    writeln!(writer, "    Visual:          {:.3}", record.fidelity.visual).map_err(stdout_error)?;
    if !record.fidelity.reasons.is_empty() {
        writeln!(
            writer,
            "    Reasons:         {}",
            record.fidelity.reasons.join(", ")
        )
        .map_err(stdout_error)?;
    }
    if let Some(ref fp) = record.render_fingerprint {
        writeln!(writer, "  Render Hash:       {}", fp).map_err(stdout_error)?;
    }
    if !record.text_fragment.is_empty() {
        writeln!(writer, "  Source Fragment:   {:?}", record.text_fragment)
            .map_err(stdout_error)?;
    }
    emit_warnings(&warnings, args.quiet, args.json_errors)
}

fn document_glyph_coverage(doc: &Document, source: &DocumentSource) -> f32 {
    let mut text = String::new();
    for block in &doc.blocks {
        text.push_str(&block.text());
    }
    match source.format() {
        DocumentFormat::Docx => docsight_render::glyph_coverage(&text),
        DocumentFormat::Pdf => docsight_pdf::pdf_glyph_coverage(&text),
    }
}

struct CoverageArgs<'a> {
    path: &'a Path,
    page: Option<u32>,
    regions: bool,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn coverage(args: CoverageArgs<'_>) -> Result<(), DocsightError> {
    let source = DocumentSource::open(args.path)?;
    let doc = load_document(&source)?;
    let glyph_coverage = document_glyph_coverage(&doc, &source);
    let report = compute_coverage(&doc, &source, args.page, args.regions, glyph_coverage)?;

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            args.limits.clone(),
            "coverage".into(),
            source.sha256().to_owned(),
            1 + report.pages.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let global_val =
            serde_json::to_value(&report.global).map_err(output_serialization_error)?;
        writer.write_item("coverage.global", &global_val)?;
        for page_cov in &report.pages {
            let page_val = serde_json::to_value(page_cov).map_err(output_serialization_error)?;
            if !writer.write_item("coverage.page", &page_val)? {
                break;
            }
        }
        for warning in &doc.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        return write_single_json(&source, &report, doc.warnings, args.limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Coverage Report:").map_err(stdout_error)?;
    writeln!(writer, "  Format:              {}", report.format).map_err(stdout_error)?;
    writeln!(
        writer,
        "  Overall Fidelity:    {:.3}",
        report.global.overall_fidelity
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "  Text Fidelity:       {:.3} ({})",
        report.global.text.score,
        report.global.text.status.as_str()
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "  Structure Fidelity:  {:.3} ({})",
        report.global.structure.score,
        report.global.structure.status.as_str()
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "  Geometry Fidelity:   {:.3} ({})",
        report.global.geometry.score,
        report.global.geometry.status.as_str()
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "  Visual Fidelity:     {:.3} ({})",
        report.global.visual.score,
        report.global.visual.status.as_str()
    )
    .map_err(stdout_error)?;
    writeln!(
        writer,
        "  Affected Objects:    {}",
        report.affected_objects_count
    )
    .map_err(stdout_error)?;
    if !report.reason_codes.is_empty() {
        writeln!(
            writer,
            "  Reason Codes:        {}",
            report.reason_codes.join(", ")
        )
        .map_err(stdout_error)?;
    }
    if args.regions {
        writeln!(writer, "\nAddressable Regions:").map_err(stdout_error)?;
        for page_cov in &report.pages {
            for reg in &page_cov.regions {
                let obj_str = reg
                    .object_id
                    .as_ref()
                    .map(|o| o.as_str())
                    .unwrap_or("<page>");
                writeln!(
                    writer,
                    "  * Page {:>2} [{}] {}: {}",
                    reg.page, obj_str, reg.reason_code, reg.description
                )
                .map_err(stdout_error)?;
            }
        }
    }
    emit_warnings(&doc.warnings, args.quiet, args.json_errors)
}

struct HitArgs<'a> {
    path: &'a Path,
    page: u32,
    point: Option<&'a str>,
    bbox: Option<&'a str>,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn parse_point(input: &str) -> Result<(f32, f32), DocsightError> {
    let parts: Vec<&str> = input.split(',').map(|s| s.trim()).collect();
    if parts.len() != 2 {
        return Err(DocsightError::InvalidArgument {
            message: "point must be formatted as x,y".to_owned(),
        });
    }
    let x: f32 = parts[0]
        .parse()
        .map_err(|_| DocsightError::InvalidArgument {
            message: format!("invalid x coordinate: {}", parts[0]),
        })?;
    let y: f32 = parts[1]
        .parse()
        .map_err(|_| DocsightError::InvalidArgument {
            message: format!("invalid y coordinate: {}", parts[1]),
        })?;
    Ok((x, y))
}

fn hit(args: HitArgs<'_>) -> Result<(), DocsightError> {
    let query = match (args.point, args.bbox) {
        (Some(pt), None) => {
            let (x, y) = parse_point(pt)?;
            HitQuery::Point(x, y)
        }
        (None, Some(bb)) => {
            let rect =
                parse_bbox(bb).map_err(|msg| DocsightError::InvalidArgument { message: msg })?;
            HitQuery::BBox(rect)
        }
        (Some(_), Some(_)) => {
            return Err(DocsightError::InvalidArgument {
                message: "cannot provide both --point and --bbox".to_owned(),
            });
        }
        (None, None) => {
            return Err(DocsightError::InvalidArgument {
                message: "either --point <x,y> or --bbox <x0,y0,x1,y1> must be provided".to_owned(),
            });
        }
    };

    let source = DocumentSource::open(args.path)?;
    let doc = load_document(&source)?;
    let result = docsight_render::hit_test(&doc, args.page, &query)?;

    if args.ndjson {
        return write_single_ndjson(&source, "hit", "hit", &result, &doc.warnings, args.limits);
    }

    if args.json {
        return write_single_json(&source, &result, doc.warnings, args.limits);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match query {
        HitQuery::Point(x, y) => {
            writeln!(
                writer,
                "Hit Test on Page {} at point ({:.1}, {:.1}):",
                args.page, x, y
            )
            .map_err(stdout_error)?;
        }
        HitQuery::BBox(rect) => {
            writeln!(
                writer,
                "Hit Test on Page {} in bbox [{:.1}, {:.1}, {:.1}, {:.1}]:",
                args.page, rect.x0, rect.y0, rect.x1, rect.y1
            )
            .map_err(stdout_error)?;
        }
    }
    writeln!(writer, "  Hits: {}", result.total_hits).map_err(stdout_error)?;
    for (idx, target) in result.targets.iter().enumerate() {
        writeln!(
            writer,
            "  {}. [{}] {:?} (z: {}, order: {})",
            idx + 1,
            target.object_id,
            target.kind,
            target.z_index,
            target.reading_order
        )
        .map_err(stdout_error)?;
        writeln!(writer, "     Source:   {}", target.source_path).map_err(stdout_error)?;
        writeln!(
            writer,
            "     BBox:     [{:.1}, {:.1}, {:.1}, {:.1}]",
            target.bbox.x0, target.bbox.y0, target.bbox.x1, target.bbox.y1
        )
        .map_err(stdout_error)?;
        if let Some(ref cell) = target.cell {
            writeln!(
                writer,
                "     Cell:     row {}, col {} (span {}x{})",
                cell.row, cell.column, cell.row_span, cell.column_span
            )
            .map_err(stdout_error)?;
        }
        if !target.text_snippet.is_empty() {
            writeln!(writer, "     Snippet:  {:?}", target.text_snippet).map_err(stdout_error)?;
        }
    }

    emit_warnings(&doc.warnings, args.quiet, args.json_errors)
}
