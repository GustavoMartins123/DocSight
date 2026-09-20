mod cache;
pub(crate) mod mcp;

use cache::{CacheAction, CacheSettings, DocumentLoader, Reporting};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell};
use docsight_agent::{
    AgentEnvelope, AgentErrorEnvelope, NdjsonWriter, OutputLimits, ProjectionProfile, QueryLimits,
    adaptive_agent_envelope, apply_bounded_collection, project_json, truncate_json_text_fields,
    validate_projection,
};
use docsight_core::{
    BlockContent, CoverageStatus, Diagnostic, DiagnosticSeverity, DocsightError, Document,
    DocumentFormat, DocumentSource, ObjectId, PageFidelity, Rect, compute_coverage,
    compute_evidence, document_capabilities, table_to_csv, table_to_html, table_to_markdown,
    table_to_tsv, table_to_tsv_string,
};
use docsight_diff::{
    DiffDocumentIdentity, DiffOptions, DiffSummary, VisualDiff, diff_documents_with_passwords,
};
use docsight_pdf::{ENGINE_NAME, PdfDocument};
use docsight_render::{
    HitQuery, RenderRequest, RenderTarget, render_document_with_password,
    trace::{
        TraceDecisionCoverage, TraceTarget, create_proof_bundle_with_password, read_proof_bundle,
        read_trace, record_trace_with_password, verify_proof_bundle_with_password,
        verify_trace_with_password,
    },
};
use docsight_search::{
    FindMode, FindObjectKind, FindRequest, FindResult, PageRange, PeekResult, ResolveCandidate,
    ResolveReason, ResolveResult, ResolveStatus, SemanticKind, SemanticObject, SemanticViewport,
    SpatialQueryResult, TextMatch, ViewportObject, ViewportRole, context_neighborhood,
    execute_spatial_query, find as find_occurrences, focus_object, focus_pages,
    overview as document_overview, peek_object, peek_pages, peek_section,
    resolve as resolve_descriptor,
};
use serde::Serialize;
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
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

    #[arg(
        long,
        global = true,
        value_name = "BYTES",
        value_parser = parse_byte_budget,
        help = "Override maximum document size in bytes, such as 128mb"
    )]
    max_document_bytes: Option<usize>,

    #[arg(
        long,
        global = true,
        value_parser = parse_byte_budget,
        help = "Adaptive serialized-output byte budget, such as 4kb or 24kb"
    )]
    budget: Option<usize>,

    #[arg(
        long,
        global = true,
        value_enum,
        help = "Fixed compact, balanced or rich evidence projection"
    )]
    budget_profile: Option<BudgetProfile>,

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

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Read the PDF password from a bounded single-line file"
    )]
    password_file: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_name = "PASSWORD",
        help = "Supply the PDF password directly"
    )]
    password: Option<String>,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Reuse parsed document IR from an opt-in content-addressed cache directory"
    )]
    cache_dir: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_name = "BYTES",
        value_parser = parse_byte_budget,
        help = "Maximum total cache entry bytes, such as 64mb (default 256mb)"
    )]
    cache_max_bytes: Option<usize>,

    #[arg(
        long,
        global = true,
        value_name = "COUNT",
        help = "Maximum number of cache entries (default 4096)"
    )]
    cache_max_entries: Option<u64>,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn max_document_bytes(&self) -> u64 {
        self.max_document_bytes
            .map(|b| b as u64)
            .unwrap_or(docsight_core::MAX_INSPECT_BYTES)
    }

    fn query_limits(&self) -> QueryLimits {
        QueryLimits {
            max_bytes: self.max_bytes,
            max_items: self.max_items,
            text_limit: self.text_limit,
            continue_token: self.continue_token.clone(),
            select: self.select.clone(),
            budget_bytes: self.budget,
            budget_profile: self.budget_profile.map(Into::into),
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
            || self.budget.is_some()
            || self.budget_profile.is_some()
    }

    fn quiet_mode(&self) -> bool {
        self.quiet || self.agent
    }

    fn structured_errors(&self) -> bool {
        self.json_errors || self.agent
    }

    fn reporting(&self) -> Reporting {
        Reporting {
            quiet: self.quiet_mode(),
            json_errors: self.structured_errors(),
        }
    }

    fn cache_settings(&self) -> Option<CacheSettings> {
        self.cache_dir.as_deref().map(|directory| {
            CacheSettings::new(directory, self.cache_max_bytes, self.cache_max_entries)
        })
    }

    fn has_machine_output_limits(&self) -> bool {
        self.max_bytes.is_some()
            || self.max_items.is_some()
            || self.text_limit.is_some()
            || self.continue_token.is_some()
            || self.select.is_some()
            || self.budget.is_some()
            || self.budget_profile.is_some()
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = SUMMARY_CAPABILITIES)]
    Capabilities {
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_COMPLETIONS)]
    Completions { shell: Shell },
    #[command(about = SUMMARY_INSPECT)]
    Inspect {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_OUTLINE)]
    Outline {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_TEXT)]
    Text {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_TABLES)]
    Tables {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_TABLE)]
    Table {
        path: PathBuf,
        object: String,
        #[arg(long, value_enum, default_value_t = TableFormat::Markdown)]
        format: TableFormat,
    },
    #[command(about = SUMMARY_PAGE)]
    Page {
        path: PathBuf,
        page: u32,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_RENDER)]
    Render {
        path: PathBuf,
        #[arg(long)]
        page: u32,
        #[arg(long, default_value_t = 144)]
        dpi: u16,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        trace: Option<PathBuf>,
    },
    #[command(about = SUMMARY_CROP)]
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
    #[command(about = SUMMARY_IMAGES)]
    Images {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_LINKS)]
    Links {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_DIFF)]
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
    #[command(about = SUMMARY_FINGERPRINT)]
    Fingerprint {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_EVIDENCE)]
    Evidence {
        path: PathBuf,
        object: String,
        #[arg(long, default_value_t = 144)]
        render_dpi: u16,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_BUNDLE)]
    Bundle {
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
        include_crop: bool,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_REPLAY)]
    Replay {
        trace: PathBuf,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_VERIFY)]
    Verify {
        bundle: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_COVERAGE)]
    Coverage {
        path: PathBuf,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long)]
        regions: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_HIT)]
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
    #[command(about = SUMMARY_QUERY)]
    Query {
        path: PathBuf,
        expression: String,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_FIND)]
    Find {
        path: PathBuf,
        pattern: String,
        #[arg(long)]
        regex: bool,
        #[arg(long)]
        ignore_case: bool,
        #[arg(long, value_enum, value_delimiter = ',')]
        kind: Vec<FindKindArg>,
        #[arg(long, value_parser = parse_page_range)]
        pages: Option<PageRange>,
        #[arg(long, value_parser = parse_bbox)]
        bbox: Option<Rect>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_OVERVIEW)]
    Overview {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_FOCUS)]
    Focus {
        path: PathBuf,
        target: Option<String>,
        #[arg(long, value_parser = parse_page_range)]
        pages: Option<PageRange>,
        #[arg(long)]
        related: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_PEEK)]
    Peek {
        path: PathBuf,
        #[arg(long)]
        page: Option<u32>,
        #[arg(long, value_parser = parse_page_range)]
        pages: Option<PageRange>,
        #[arg(long)]
        object: Option<String>,
        #[arg(long)]
        section: Option<u32>,
        #[arg(long)]
        related: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_CONTEXT)]
    Context {
        path: PathBuf,
        object: Option<String>,
        #[arg(long)]
        find: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<InteractionKind>,
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_value = "content,neighbors,geometry,fidelity,provenance,heading,related"
        )]
        include: Vec<ContextInclude>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_CACHE)]
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
    #[command(about = SUMMARY_RESOLVE)]
    Resolve {
        path: PathBuf,
        #[arg(long)]
        text: String,
        #[arg(long, value_enum)]
        kind: Option<InteractionKind>,
        #[arg(long, value_parser = parse_page_range)]
        pages: Option<PageRange>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = SUMMARY_MCP)]
    Mcp,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    #[command(about = "report entry, quarantine and temporary file counts")]
    Stats {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "validate every entry and quarantine invalid ones")]
    Verify {
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "remove quarantine, stale temporaries and entries from other engines, then enforce limits"
    )]
    Prune {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "remove every entry, quarantined file and temporary file")]
    Clear {
        #[arg(long)]
        json: bool,
    },
}

impl CacheCommand {
    fn action(&self) -> (CacheAction, bool) {
        match self {
            Self::Stats { json } => (CacheAction::Stats, *json),
            Self::Verify { json } => (CacheAction::Verify, *json),
            Self::Prune { json } => (CacheAction::Prune, *json),
            Self::Clear { json } => (CacheAction::Clear, *json),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum TableFormat {
    Json,
    Markdown,
    Csv,
    Html,
    Tsv,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum BudgetProfile {
    Compact,
    Balanced,
    Rich,
}

impl From<BudgetProfile> for ProjectionProfile {
    fn from(value: BudgetProfile) -> Self {
        match value {
            BudgetProfile::Compact => Self::Compact,
            BudgetProfile::Balanced => Self::Balanced,
            BudgetProfile::Rich => Self::Rich,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
enum ContextInclude {
    Content,
    Neighbors,
    Geometry,
    Fidelity,
    Provenance,
    Heading,
    Related,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum FindKindArg {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    TableCell,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
    Hyperlink,
}

impl FindKindArg {
    fn to_find_kind(self) -> FindObjectKind {
        match self {
            Self::Paragraph => FindObjectKind::Paragraph,
            Self::Heading => FindObjectKind::Heading,
            Self::ListItem => FindObjectKind::ListItem,
            Self::Table => FindObjectKind::Table,
            Self::Figure => FindObjectKind::Figure,
            Self::Shape => FindObjectKind::Shape,
            Self::Note => FindObjectKind::Note,
            Self::Unknown => FindObjectKind::Unknown,
            Self::TableCell => FindObjectKind::TableCell,
            Self::Header => FindObjectKind::Header,
            Self::Footer => FindObjectKind::Footer,
            Self::Watermark => FindObjectKind::Watermark,
            Self::CommentMarker => FindObjectKind::CommentMarker,
            Self::Annotation => FindObjectKind::Annotation,
            Self::Hyperlink => FindObjectKind::Hyperlink,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum InteractionKind {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
}

impl From<InteractionKind> for SemanticKind {
    fn from(value: InteractionKind) -> Self {
        match value {
            InteractionKind::Paragraph => Self::Paragraph,
            InteractionKind::Heading => Self::Heading,
            InteractionKind::ListItem => Self::ListItem,
            InteractionKind::Table => Self::Table,
            InteractionKind::Figure => Self::Figure,
            InteractionKind::Shape => Self::Shape,
            InteractionKind::Note => Self::Note,
            InteractionKind::Unknown => Self::Unknown,
            InteractionKind::Header => Self::Header,
            InteractionKind::Footer => Self::Footer,
            InteractionKind::Watermark => Self::Watermark,
            InteractionKind::CommentMarker => Self::CommentMarker,
            InteractionKind::Annotation => Self::Annotation,
        }
    }
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

impl CapabilityAssessment {
    fn from_status(status: CoverageStatus) -> Self {
        Self {
            available: status != CoverageStatus::Unsupported,
            source_faithful: status == CoverageStatus::Exact,
            fidelity: status.as_str(),
        }
    }

    fn unsupported() -> Self {
        Self::from_status(CoverageStatus::Unsupported)
    }
}

#[derive(Clone, Debug, Serialize)]
struct InspectCapabilityDetails {
    structure: CapabilityAssessment,
    text: CapabilityAssessment,
    render: CapabilityAssessment,
}

impl InspectCapabilityDetails {
    fn from_document(document: &Document) -> Self {
        let capabilities = document_capabilities(document);
        Self {
            structure: CapabilityAssessment::from_status(capabilities.structure),
            text: CapabilityAssessment::from_status(capabilities.text),
            render: CapabilityAssessment::from_status(capabilities.render),
        }
    }

    fn unsupported() -> Self {
        Self {
            structure: CapabilityAssessment::unsupported(),
            text: CapabilityAssessment::unsupported(),
            render: CapabilityAssessment::unsupported(),
        }
    }

    fn available(&self) -> InspectCapabilities {
        InspectCapabilities {
            structure: self.structure.available,
            text: self.text.available,
            render: self.render.available,
        }
    }

    fn source_faithful(&self) -> InspectCapabilities {
        InspectCapabilities {
            structure: self.structure.source_faithful,
            text: self.text.source_faithful,
            render: self.render.source_faithful,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InspectResult {
    pub(crate) format: DocumentFormat,
    pub(crate) size_bytes: u64,
    pub(crate) capabilities: InspectCapabilities,
    pub(crate) source_faithful: InspectCapabilities,
    pub(crate) capability_details: InspectCapabilityDetails,
    pub(crate) blocks_by_kind: BTreeMap<&'static str, usize>,
    pub(crate) paragraphs: Option<usize>,
    pub(crate) headings: Option<usize>,
    pub(crate) tables: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) figures: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) comments: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracked: Option<docsight_core::TrackedChanges>,
    pub(crate) pages: Option<u32>,
    pub(crate) engine: Option<&'static str>,
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
    page_fidelity: PageFidelity,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PageSpanRecord {
    pub(crate) id: ObjectId,
    pub(crate) text: String,
    pub(crate) bbox: Rect,
    pub(crate) reading_order: u32,
    pub(crate) confidence: f32,
    pub(crate) source: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) continued: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PageOverlayRecord {
    pub(crate) id: ObjectId,
    pub(crate) kind: docsight_core::OverlayKind,
    pub(crate) text: String,
    pub(crate) bbox: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PageResult {
    pub(crate) number: u32,
    pub(crate) width_pt: f32,
    pub(crate) height_pt: f32,
    pub(crate) spans: Vec<PageSpanRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) overlays: Vec<PageOverlayRecord>,
    pub(crate) page_fidelity: PageFidelity,
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
    invocation: &'static str,
    formats: &'static [&'static str],
    ndjson: bool,
    ndjson_events: &'static [&'static str],
    bounded: bool,
    result_schema: Option<&'static str>,
    result_root: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
struct AgentSandboxCapability {
    flag: &'static str,
    agent_default: bool,
    recommended_for_untrusted_input: bool,
    supported_platforms: &'static [&'static str],
    enforced_controls: &'static [&'static str],
    unsupported_platform_behavior: &'static str,
    failure_mode: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct AgentPdfPasswordCapability {
    flag: &'static str,
    applies_to: &'static [&'static str],
    transport: &'static str,
    file_format: &'static str,
    maximum_password_bytes: usize,
    secret_in_argv: bool,
    secret_persisted: bool,
    failure_mode: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct AgentCacheCapability {
    flag: &'static str,
    limit_flags: &'static [&'static str],
    agent_default: bool,
    artifact: &'static str,
    layout: &'static str,
    applies_to_commands: &'static [&'static str],
    maintenance_command: &'static str,
    maintenance_actions: &'static [&'static str],
    key_components: &'static [&'static str],
    default_max_bytes: u64,
    default_max_entries: u64,
    write_mode: &'static str,
    invalid_entry_behavior: &'static str,
    sandbox_behavior: &'static str,
    password_file_behavior: &'static str,
    output_identity: &'static str,
    failure_mode: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct AgentErrorContract {
    channel: &'static str,
    schema: &'static str,
    success_exit_code: u8,
    codes: &'static [docsight_core::ErrorCatalogEntry],
}

#[derive(Clone, Debug, Serialize)]
struct AgentCapabilitiesResult {
    profile: &'static str,
    protocol: &'static str,
    error_schema: &'static str,
    error_channel: &'static str,
    errors: AgentErrorContract,
    invocation_prefix: &'static str,
    sandbox: AgentSandboxCapability,
    pdf_password: AgentPdfPasswordCapability,
    cache: AgentCacheCapability,
    document_formats: &'static [&'static str],
    output_modes: &'static [&'static str],
    agent_defaults: &'static str,
    limits: &'static [&'static str],
    ingestion_limits: &'static [docsight_ingest::IngestionLimit],
    coordinate_system: &'static str,
    commands: Vec<CommandCapability>,
}

#[derive(Clone, Debug, Serialize)]
struct CapabilitiesEnvelope {
    schema: &'static str,
    engine: &'static str,
    result: AgentCapabilitiesResult,
}

#[derive(Clone, Debug, Serialize)]
struct QueryNdjsonSummary {
    query: String,
    total_matches: usize,
    geometry_unavailable_objects: usize,
}

#[derive(Clone, Debug, Serialize)]
struct OverviewNdjsonSummary {
    format: DocumentFormat,
    page_count: usize,
    counts: docsight_search::OverviewCounts,
    total_landmarks: usize,
}

#[derive(Clone, Debug, Serialize)]
struct FocusNdjsonSummary {
    target: docsight_search::ViewportTarget,
    scope_pages: Vec<u32>,
    total_objects: usize,
}

#[derive(Clone, Debug, Serialize)]
struct PeekNdjsonSummary {
    target: docsight_search::PeekTarget,
    scope_pages: Vec<u32>,
    total_objects: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ResolveNdjsonSummary {
    query: docsight_search::ResolveQuery,
    status: ResolveStatus,
    total_candidates: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ContextSelectionMode {
    ExplicitObject,
    Find,
}

#[derive(Clone, Debug, Serialize)]
struct ContextSelection {
    mode: ContextSelectionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    descriptor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chosen_object: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_range: Option<TextMatch>,
    reasons: Vec<ResolveReason>,
}

#[derive(Clone, Debug, Serialize)]
struct ContextPage {
    number: u32,
    width_pt: f32,
    height_pt: f32,
}

#[derive(Clone, Debug, Serialize)]
struct ContextSection {
    id: ObjectId,
    index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ContextSectionStatus {
    Exact,
    NotApplicable,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
struct ContextContainers {
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<ContextPage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    section: Option<ContextSection>,
    section_status: ContextSectionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    section_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object: Option<ContextContainerObject>,
}

#[derive(Clone, Debug, Serialize)]
struct ContextContainerObject {
    id: ObjectId,
    kind: docsight_core::BlockKind,
}

fn context_content(
    object: &docsight_core::DocumentObject<'_>,
) -> Result<serde_json::Value, DocsightError> {
    use docsight_core::DocumentObject;
    match object {
        DocumentObject::Block { block, .. } => {
            serde_json::to_value(&block.content).map_err(output_serialization_error)
        }
        DocumentObject::TableCell { cell, .. } => Ok(serde_json::json!({
            "type": "table_cell",
            "row": cell.row,
            "column": cell.column,
            "row_span": cell.row_span,
            "column_span": cell.column_span,
            "text": cell.text,
            "blocks": cell.blocks,
        })),
        DocumentObject::Overlay(overlay) => Ok(serde_json::json!({
            "type": "overlay",
            "kind": overlay.kind,
            "text": overlay.text
        })),
        DocumentObject::Hyperlink(link) => Ok(serde_json::json!({
            "type": "hyperlink",
            "target": link.target,
            "is_external": link.is_external,
            "text": link.text,
        })),
    }
}

#[derive(Clone, Debug, Serialize)]
struct ContextRelatedObject {
    role: ViewportRole,
    confidence: f32,
    provenance: String,
    object: SemanticObject,
}

#[derive(Clone, Debug, Serialize)]
struct ContextGeometry {
    available: bool,
    coordinate_system: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bbox: Option<Rect>,
    z_index: i32,
    reading_order: u32,
}

#[derive(Clone, Debug, Serialize)]
struct ContextFidelity {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structure: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geometry: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    visual: Option<f32>,
    reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ContextProvenance {
    source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_length: Option<u64>,
    confidence: f32,
}

#[derive(Clone, Debug, Serialize)]
struct ContextPackage {
    target: SemanticObject,
    containers: ContextContainers,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<ContextRelatedObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    neighbors: Option<Vec<ContextRelatedObject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    related: Option<Vec<ContextRelatedObject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geometry: Option<ContextGeometry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fidelity: Option<ContextFidelity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<ContextProvenance>,
}

#[derive(Clone, Debug, Serialize)]
struct ContextResult {
    status: ResolveStatus,
    selection: ContextSelection,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<ContextPackage>,
    total_candidates: usize,
    candidates: Vec<ResolveCandidate>,
}

const MAX_PDF_PASSWORD_BYTES: usize = 127;

struct PdfPassword(Vec<u8>);

impl PdfPassword {
    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for PdfPassword {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn read_pdf_password(path: &Path) -> Result<PdfPassword, DocsightError> {
    let file = std::fs::File::open(path).map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(DocsightError::InvalidArgument {
            message: "PDF password path must identify a regular file".to_owned(),
        });
    }
    let maximum_file_bytes =
        u64::try_from(MAX_PDF_PASSWORD_BYTES + 2).map_err(|_| DocsightError::ResourceLimit {
            resource: "PDF password file bytes".to_owned(),
            limit: u64::MAX,
        })?;
    if metadata.len() > maximum_file_bytes {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF password file bytes".to_owned(),
            limit: maximum_file_bytes,
        });
    }
    let mut password = PdfPassword(Vec::with_capacity(MAX_PDF_PASSWORD_BYTES + 2));
    file.take(maximum_file_bytes + 1)
        .read_to_end(&mut password.0)
        .map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if password.0.len() > MAX_PDF_PASSWORD_BYTES + 2 {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF password file bytes".to_owned(),
            limit: maximum_file_bytes,
        });
    }
    if password.0.last() == Some(&b'\n') {
        password.0.pop();
        if password.0.last() == Some(&b'\r') {
            password.0.pop();
        }
    }
    if password.0.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "PDF password file must contain a non-empty password".to_owned(),
        });
    }
    if password.0.contains(&b'\n') || password.0.contains(&b'\r') {
        return Err(DocsightError::InvalidArgument {
            message: "PDF password file must contain exactly one line".to_owned(),
        });
    }
    if password.0.len() > MAX_PDF_PASSWORD_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF password bytes".to_owned(),
            limit: MAX_PDF_PASSWORD_BYTES as u64,
        });
    }
    Ok(password)
}

fn parse_direct_password(secret: &str) -> Result<PdfPassword, DocsightError> {
    if secret.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "PDF password must not be empty".to_owned(),
        });
    }
    if secret.contains('\n') || secret.contains('\r') {
        return Err(DocsightError::InvalidArgument {
            message: "PDF password must contain exactly one line".to_owned(),
        });
    }
    if secret.len() > MAX_PDF_PASSWORD_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF password bytes".to_owned(),
            limit: MAX_PDF_PASSWORD_BYTES as u64,
        });
    }
    Ok(PdfPassword(secret.as_bytes().to_vec()))
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
        if let Err(error) = validate_cache_arguments(&cli) {
            let exit_code = error.exit_code();
            return if emit_error(&error, cli.structured_errors(), cli.agent).is_ok() {
                ExitCode::from(exit_code)
            } else {
                ExitCode::from(40)
            };
        }
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
        let handoff = match (cli.cache_settings(), cached_document_path(&cli.command)) {
            (Some(settings), Some(document)) => cache::SandboxCacheHandoff::prepare(
                &settings,
                document,
                &exe,
                cli.reporting(),
                cli.max_document_bytes(),
            ),
            _ => Ok(None),
        };
        let environment = handoff.and_then(|handoff| {
            let mut environment = vec![(
                docsight_worker::SANDBOX_CHILD_ENV.to_owned(),
                "1".to_owned(),
            )];
            if let Some(handoff) = &handoff {
                environment.extend(handoff.environment()?);
            }
            Ok((handoff, environment))
        });
        let (handoff, environment) = match environment {
            Ok(prepared) => prepared,
            Err(error) => {
                let exit_code = error.exit_code();
                return if emit_error(&error, cli.structured_errors(), cli.agent).is_ok() {
                    ExitCode::from(exit_code)
                } else {
                    ExitCode::from(40)
                };
            }
        };
        let raw_args = cache::strip_cache_arguments(
            std::env::args()
                .skip(1)
                .filter(|arg| arg != "--sandbox")
                .collect(),
        );
        match docsight_worker::run_in_sandbox_with_env(
            Some(&exe),
            &docsight_worker::SandboxPolicy::default(),
            &raw_args,
            &environment,
        ) {
            Ok(output) => {
                if let Some(handoff) = handoff
                    && let Err(error) = handoff.commit(cli.reporting())
                {
                    let exit_code = error.exit_code();
                    if emit_error(&error, cli.structured_errors(), cli.agent).is_err() {
                        return ExitCode::from(40);
                    }
                    return ExitCode::from(exit_code);
                }
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

fn cached_document_path(command: &Command) -> Option<&Path> {
    match command {
        Command::Inspect { path, .. }
        | Command::Outline { path, .. }
        | Command::Text { path, .. }
        | Command::Tables { path, .. }
        | Command::Table { path, .. }
        | Command::Page { path, .. }
        | Command::Images { path, .. }
        | Command::Links { path, .. }
        | Command::Evidence { path, .. }
        | Command::Coverage { path, .. }
        | Command::Hit { path, .. }
        | Command::Query { path, .. }
        | Command::Find { path, .. }
        | Command::Overview { path, .. }
        | Command::Focus { path, .. }
        | Command::Peek { path, .. }
        | Command::Context { path, .. }
        | Command::Resolve { path, .. } => Some(path),
        Command::Capabilities { .. }
        | Command::Completions { .. }
        | Command::Render { .. }
        | Command::Crop { .. }
        | Command::Diff { .. }
        | Command::Fingerprint { .. }
        | Command::Bundle { .. }
        | Command::Replay { .. }
        | Command::Verify { .. }
        | Command::Cache { .. }
        | Command::Mcp => None,
    }
}

fn validate_cache_arguments(cli: &Cli) -> Result<(), DocsightError> {
    if std::env::var_os(cache::CACHE_HANDOFF_ENV).is_some()
        && std::env::var_os(docsight_worker::SANDBOX_CHILD_ENV).is_none()
    {
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "{} is reserved for sandbox workers",
                cache::CACHE_HANDOFF_ENV
            ),
        });
    }
    let maintenance = matches!(cli.command, Command::Cache { .. });
    if cli.cache_dir.is_none() {
        if cli.cache_max_bytes.is_some() || cli.cache_max_entries.is_some() {
            return Err(DocsightError::InvalidArgument {
                message: "--cache-max-bytes and --cache-max-entries require --cache-dir".to_owned(),
            });
        }
        if maintenance {
            return Err(DocsightError::InvalidArgument {
                message: "cache maintenance requires --cache-dir".to_owned(),
            });
        }
        return Ok(());
    }
    if maintenance {
        if cli.sandbox {
            return Err(DocsightError::InvalidArgument {
                message: "cache maintenance does not parse documents and cannot run with --sandbox"
                    .to_owned(),
            });
        }
        if cli.has_machine_output_limits() {
            return Err(DocsightError::InvalidArgument {
                message: "cache maintenance reports are unbounded and cannot be combined with machine-output limits"
                    .to_owned(),
            });
        }
        return Ok(());
    }
    if cached_document_path(&cli.command).is_none() {
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "--cache-dir applies only to commands that load the document IR: {}",
                cache::CACHED_COMMANDS.join(", ")
            ),
        });
    }
    if cli.password_file.is_some() {
        return Err(DocsightError::InvalidArgument {
            message: "--cache-dir cannot be combined with --password-file because decrypted document content is never persisted"
                .to_owned(),
        });
    }
    if cli.password.is_some() {
        return Err(DocsightError::InvalidArgument {
            message: "--cache-dir cannot be combined with --password because decrypted document content is never persisted"
                .to_owned(),
        });
    }
    if cli.password.is_some() && cli.password_file.is_some() {
        return Err(DocsightError::InvalidArgument {
            message: "cannot combine --password and --password-file".to_owned(),
        });
    }
    Ok(())
}

fn execute(cli: &Cli) -> Result<(), DocsightError> {
    validate_cache_arguments(cli)?;
    if cli.ndjson && (cli.budget.is_some() || cli.budget_profile.is_some()) {
        return Err(DocsightError::InvalidArgument {
            message: "--budget and --budget-profile require a bounded JSON envelope; use --max-bytes for NDJSON streams"
                .to_owned(),
        });
    }
    if matches!(cli.command, Command::Capabilities { .. })
        && (cli.budget.is_some() || cli.budget_profile.is_some())
    {
        return Err(DocsightError::InvalidArgument {
            message: "--budget and --budget-profile apply to document evidence, not capabilities"
                .to_owned(),
        });
    }
    if matches!(cli.command, Command::Completions { .. })
        && (cli.is_agent_json(false) || cli.ndjson)
    {
        return Err(DocsightError::InvalidArgument {
            message: "completions emits a shell script for human setup and cannot be combined with --agent, --ndjson or machine-output limits"
                .to_owned(),
        });
    }
    if cli.password.is_some() && cli.password_file.is_some() {
        return Err(DocsightError::InvalidArgument {
            message: "cannot combine --password and --password-file".to_owned(),
        });
    }
    if cli.password_file.is_some()
        && matches!(
            cli.command,
            Command::Capabilities { .. }
                | Command::Completions { .. }
                | Command::Fingerprint { .. }
                | Command::Cache { .. }
        )
    {
        return Err(DocsightError::InvalidArgument {
            message: "--password-file applies only to operations that decrypt PDF content"
                .to_owned(),
        });
    }
    if cli.password.is_some()
        && matches!(
            cli.command,
            Command::Capabilities { .. }
                | Command::Completions { .. }
                | Command::Fingerprint { .. }
                | Command::Cache { .. }
        )
    {
        return Err(DocsightError::InvalidArgument {
            message: "--password applies only to operations that decrypt PDF content".to_owned(),
        });
    }
    let password_storage = if let Some(secret) = &cli.password {
        Some(parse_direct_password(secret)?)
    } else {
        cli.password_file
            .as_deref()
            .map(read_pdf_password)
            .transpose()?
    };
    let password = password_storage
        .as_ref()
        .map(PdfPassword::as_bytes)
        .unwrap_or_default();
    let limits = cli.query_limits();
    let quiet = cli.quiet_mode();
    let json_errors = cli.structured_errors();
    let cache_settings = cli
        .cache_settings()
        .filter(|_| cached_document_path(&cli.command).is_some());
    let loader = DocumentLoader::for_invocation(
        password,
        cache_settings.as_ref(),
        cli.reporting(),
        cli.max_document_bytes(),
    )?;
    match &cli.command {
        Command::Capabilities { json } => capabilities(cli.is_agent_json(*json), cli.ndjson),
        Command::Cache { action } => {
            let settings = cli
                .cache_settings()
                .ok_or_else(|| DocsightError::InvalidArgument {
                    message: "cache maintenance requires --cache-dir".to_owned(),
                })?;
            let (action, json) = action.action();
            cache::cache_command(
                &settings,
                action,
                cli.is_agent_json(json),
                cli.ndjson,
                cli.reporting(),
            )
        }
        Command::Completions { shell } => completions(*shell),
        Command::Inspect { path, json } => inspect(
            path,
            &loader,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Outline { path, json } => outline(
            path,
            &loader,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Text { path, json } => document_text(
            path,
            &loader,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Tables { path, json } => tables(
            path,
            &loader,
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
            loader: &loader,
            object,
            format: *format,
            json: cli.is_agent_json(*format == TableFormat::Json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Page { path, page, json } => page_command(PageCommandArgs {
            path,
            number: *page,
            loader: &loader,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Render {
            path,
            page,
            dpi,
            out,
            trace,
        } => render(RenderCommandArgs {
            path,
            password,
            target: RenderTarget::Page { page: *page },
            dpi: *dpi,
            out,
            trace: trace.as_deref(),
            command: "render",
            json: cli.is_agent_json(false),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
            max_document_bytes: cli.max_document_bytes(),
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
                password,
                target,
                dpi: *dpi,
                out,
                trace: None,
                command: "crop",
                json: cli.is_agent_json(false),
                ndjson: cli.ndjson,
                limits: &limits,
                quiet,
                json_errors,
                max_document_bytes: cli.max_document_bytes(),
            })
        }
        Command::Images { path, json } => images(
            path,
            &loader,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
        ),
        Command::Links { path, json } => links(
            path,
            &loader,
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
            password_before: password,
            password_after: password,
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
            max_document_bytes: cli.max_document_bytes(),
        }),
        Command::Fingerprint { path, json } => fingerprint(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            quiet,
            json_errors,
            cli.max_document_bytes(),
        ),
        Command::Evidence {
            path,
            object,
            render_dpi,
            json,
        } => evidence(EvidenceArgs {
            path,
            loader: &loader,
            object,
            render_dpi: *render_dpi,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Bundle {
            path,
            page,
            bbox,
            object,
            dpi,
            include_crop,
            out,
            json,
        } => bundle(BundleArgs {
            path,
            password,
            page: *page,
            bbox: *bbox,
            object: object.as_deref(),
            dpi: *dpi,
            include_crop: *include_crop,
            out,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
            max_document_bytes: cli.max_document_bytes(),
        }),
        Command::Replay {
            trace,
            verify,
            json,
        } => replay(ReplayArgs {
            trace,
            password,
            verify: *verify,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Verify { bundle, json } => verify(VerifyArgs {
            bundle,
            password,
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
            loader: &loader,
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
            loader: &loader,
            page: *page,
            point: point.as_deref(),
            bbox: bbox.as_deref(),
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Find {
            path,
            pattern,
            regex,
            ignore_case,
            kind,
            pages,
            bbox,
            json,
        } => find_command(FindArgs {
            path,
            loader: &loader,
            pattern,
            regex: *regex,
            ignore_case: *ignore_case,
            kinds: kind,
            pages: *pages,
            bbox: *bbox,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Query {
            path,
            expression,
            json,
        } => query(QueryArgs {
            path,
            loader: &loader,
            expression,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Overview { path, json } => overview(OverviewArgs {
            path,
            loader: &loader,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Focus {
            path,
            target,
            pages,
            related,
            json,
        } => focus(FocusArgs {
            path,
            loader: &loader,
            target: target.as_deref(),
            pages: *pages,
            related: *related,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Peek {
            path,
            page,
            pages,
            object,
            section,
            related,
            json,
        } => peek(PeekArgs {
            path,
            loader: &loader,
            page: *page,
            pages: *pages,
            object: object.as_deref(),
            section: *section,
            related: *related,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Context {
            path,
            object,
            find,
            kind,
            include,
            json,
        } => context(ContextArgs {
            path,
            loader: &loader,
            object: object.as_deref(),
            find: find.as_deref(),
            kind: kind.map(Into::into),
            include,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Resolve {
            path,
            text,
            kind,
            pages,
            json,
        } => resolve(ResolveArgs {
            path,
            loader: &loader,
            text,
            kind: kind.map(Into::into),
            pages: *pages,
            json: cli.is_agent_json(*json),
            ndjson: cli.ndjson,
            limits: &limits,
            quiet,
            json_errors,
        }),
        Command::Mcp => mcp::run_mcp_server(&loader),
    }
}

const ERROR_ENVELOPE_SCHEMA: &str = "https://docsight.dev/schemas/v2/error-envelope.json";

const SUMMARY_CAPABILITIES: &str = "discover the machine contract and command surface";
const SUMMARY_MCP: &str = "run Model Context Protocol (MCP) server on stdio";
const SUMMARY_COMPLETIONS: &str = "generate a shell completion script for interactive use";
const SUMMARY_INSPECT: &str = "summarize format, counts, capabilities and fidelity";
const SUMMARY_OUTLINE: &str = "return headings in reading order";
const SUMMARY_TEXT: &str = "return text blocks and deterministic continuation";
const SUMMARY_TABLES: &str = "list structural or inferred tables with confidence";
const SUMMARY_TABLE: &str = "export one table or return its machine record";
const SUMMARY_PAGE: &str = "return page geometry, spans and overlays";
const SUMMARY_IMAGES: &str = "list figure resources and placements";
const SUMMARY_LINKS: &str = "list link metadata without fetching targets";
const SUMMARY_RENDER: &str =
    "write a PNG artifact with provenance metadata and optional deterministic trace";
const SUMMARY_CROP: &str = "write a page or object crop with provenance metadata";
const SUMMARY_DIFF: &str = "compare changes with evidence-backed cross-version lineage";
const SUMMARY_FINGERPRINT: &str = "return reproducibility inputs and result fingerprint";
const SUMMARY_EVIDENCE: &str = "return provenance and fidelity for one object";
const SUMMARY_BUNDLE: &str =
    "write a self-contained verifiable proof bundle for an object or region";
const SUMMARY_REPLAY: &str = "verify a deterministic trace using its embedded document bytes";
const SUMMARY_VERIFY: &str = "verify a self-contained proof bundle offline";
const SUMMARY_COVERAGE: &str = "return per-dimension fidelity and reason codes";
const SUMMARY_HIT: &str = "resolve a point or region to document objects";
const SUMMARY_QUERY: &str = "run constrained structural and spatial DQL selectors in page points";
const SUMMARY_FIND: &str =
    "find every literal or regex occurrence at object granularity with page-local geometry";
const SUMMARY_OVERVIEW: &str =
    "return bounded headings, tables and figures for document navigation";
const SUMMARY_FOCUS: &str = "return a bounded semantic neighborhood around an object or page range";
const SUMMARY_PEEK: &str =
    "return a compact structural projection for a page, range, object or section";
const SUMMARY_CONTEXT: &str =
    "aggregate selected content, neighborhood, geometry, fidelity and provenance";
const SUMMARY_CACHE: &str =
    "inspect, verify, prune or clear the opt-in content-addressed document IR cache";
const SUMMARY_RESOLVE: &str =
    "rank deterministic descriptor matches with explainable component scores";

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
    "--budget",
    "--budget-profile",
];
const DEFAULT_QUERY_ITEMS: usize = 100;
const DEFAULT_VIEWPORT_ITEMS: usize = 64;
const DEFAULT_PEEK_ITEMS: usize = 32;
const DEFAULT_RESOLVE_ITEMS: usize = 20;

fn bounded_machine_limits(limits: &QueryLimits, default_max_items: usize) -> QueryLimits {
    let mut effective = limits.clone();
    if effective.max_items.is_none() {
        effective.max_items = Some(default_max_items);
    }
    effective
}

fn capabilities(json: bool, ndjson: bool) -> Result<(), DocsightError> {
    let result = AgentCapabilitiesResult {
        profile: "agent-first-v1",
        protocol: docsight_agent::AGENT_SCHEMA,
        error_schema: ERROR_ENVELOPE_SCHEMA,
        error_channel: "stderr",
        errors: AgentErrorContract {
            channel: "stderr",
            schema: ERROR_ENVELOPE_SCHEMA,
            success_exit_code: docsight_core::SUCCESS_EXIT_CODE,
            codes: docsight_core::ERROR_CATALOG,
        },
        invocation_prefix: "docsight --agent",
        sandbox: AgentSandboxCapability {
            flag: "--sandbox",
            agent_default: false,
            recommended_for_untrusted_input: true,
            supported_platforms: &["linux", "macos", "windows"],
            enforced_controls: &[
                "memory",
                "cpu",
                "network",
                "filesystem",
                "isolated_temp_directory",
                "bounded_output",
            ],
            unsupported_platform_behavior: "reject",
            failure_mode: "fail_closed",
        },
        pdf_password: AgentPdfPasswordCapability {
            flag: "--password-file",
            applies_to: &["pdf"],
            transport: "file",
            file_format: "one byte string line with an optional LF or CRLF terminator",
            maximum_password_bytes: MAX_PDF_PASSWORD_BYTES,
            secret_in_argv: false,
            secret_persisted: false,
            failure_mode: "reject",
        },
        cache: AgentCacheCapability {
            flag: "--cache-dir",
            limit_flags: &["--cache-max-bytes", "--cache-max-entries"],
            agent_default: false,
            artifact: "document-ir",
            layout: cache::CACHE_LAYOUT,
            applies_to_commands: cache::CACHED_COMMANDS,
            maintenance_command: "cache",
            maintenance_actions: cache::CACHE_MAINTENANCE_ACTIONS,
            key_components: cache::CACHE_KEY_COMPONENTS,
            default_max_bytes: docsight_cache::DEFAULT_CACHE_MAX_BYTES,
            default_max_entries: docsight_cache::DEFAULT_CACHE_MAX_ENTRIES,
            write_mode: "atomic_no_clobber",
            invalid_entry_behavior: "quarantine_and_reparse",
            sandbox_behavior: "parent_process_owns_cache",
            password_file_behavior: "reject",
            output_identity: "byte_identical",
            failure_mode: "fail_closed",
        },
        document_formats: ALL_DOCUMENT_FORMATS,
        output_modes: OUTPUT_MODES,
        agent_defaults: "JSON on stdout, no diagnostics on stderr, structured errors on stderr",
        limits: AGENT_LIMITS,
        ingestion_limits: docsight_ingest::INGESTION_LIMITS,
        coordinate_system: "points at 1/72 inch with page origin at the top-left",
        commands: vec![
            CommandCapability {
                name: "capabilities",
                summary: SUMMARY_CAPABILITIES,
                invocation: "capabilities",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: true,
                ndjson_events: &["capabilities"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/capabilities-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "completions",
                summary: SUMMARY_COMPLETIONS,
                invocation: "completions <bash|elvish|fish|powershell|zsh>",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: false,
                ndjson_events: &[],
                bounded: false,
                result_schema: None,
                result_root: None,
            },
            CommandCapability {
                name: "inspect",
                summary: SUMMARY_INSPECT,
                invocation: "inspect <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["inspect"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/inspect-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "outline",
                summary: SUMMARY_OUTLINE,
                invocation: "outline <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["heading"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/outline-result.json"),
                result_root: Some("headings"),
            },
            CommandCapability {
                name: "text",
                summary: SUMMARY_TEXT,
                invocation: "text <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["block"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/text-result.json"),
                result_root: Some("blocks"),
            },
            CommandCapability {
                name: "tables",
                summary: SUMMARY_TABLES,
                invocation: "tables <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["table"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/tables-result.json"),
                result_root: Some("tables"),
            },
            CommandCapability {
                name: "table",
                summary: SUMMARY_TABLE,
                invocation: "table <path> <object> [--format json|markdown|csv|html|tsv]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["table"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/table-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "page",
                summary: SUMMARY_PAGE,
                invocation: "page <path> <page>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["span", "overlay"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/page-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "images",
                summary: SUMMARY_IMAGES,
                invocation: "images <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["image"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/images-result.json"),
                result_root: Some("images"),
            },
            CommandCapability {
                name: "links",
                summary: SUMMARY_LINKS,
                invocation: "links <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["link"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/links-result.json"),
                result_root: Some("links"),
            },
            CommandCapability {
                name: "render",
                summary: SUMMARY_RENDER,
                invocation: "render <path> --page <page> --out <png> [--dpi <dpi>] [--trace <dstrace>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["render"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/render-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "crop",
                summary: SUMMARY_CROP,
                invocation: "crop <path> (--object <id> | --page <page> --bbox <x0,y0,x1,y1>) --out <png> [--dpi <dpi>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["crop"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/render-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "diff",
                summary: SUMMARY_DIFF,
                invocation: "diff <before> <after> [--visual] [--dpi <dpi>] [--threshold <0..255>] [--out-dir <directory>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &[
                    "diff.summary",
                    "diff.visual",
                    "diff.visual.page",
                    "diff.lineage",
                    "diff.semantic",
                ],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/diff-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "fingerprint",
                summary: SUMMARY_FINGERPRINT,
                invocation: "fingerprint <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["fingerprint"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/fingerprint-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "evidence",
                summary: SUMMARY_EVIDENCE,
                invocation: "evidence <path> <object> [--render-dpi <dpi>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["evidence"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/evidence-record.json"),
                result_root: None,
            },
            CommandCapability {
                name: "bundle",
                summary: SUMMARY_BUNDLE,
                invocation: "bundle <path> (--object <id> | --page <page> --bbox <x0,y0,x1,y1>) --out <dse> [--include-crop] [--dpi <dpi>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["bundle"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/bundle-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "replay",
                summary: SUMMARY_REPLAY,
                invocation: "replay <dstrace> --verify",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: true,
                ndjson_events: &["replay"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/replay-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "verify",
                summary: SUMMARY_VERIFY,
                invocation: "verify <dse>",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: true,
                ndjson_events: &["verify"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/verify-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "coverage",
                summary: SUMMARY_COVERAGE,
                invocation: "coverage <path> [--page <page>] [--regions]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["coverage.global", "coverage.page"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/coverage-report.json"),
                result_root: None,
            },
            CommandCapability {
                name: "hit",
                summary: SUMMARY_HIT,
                invocation: "hit <path> --page <page> (--point <x,y> | --bbox <x0,y0,x1,y1>)",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["hit"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/hit-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "query",
                summary: SUMMARY_QUERY,
                invocation: "query <path> <expression>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["query.summary", "query.match"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/spatial-query-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "find",
                summary: SUMMARY_FIND,
                invocation: "find <path> <pattern> [--regex] [--ignore-case] [--kind <kinds>] [--pages <start..end>] [--bbox <x0,y0,x1,y1>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["find.summary", "find.match"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/find-result.json"),
                result_root: Some("matches"),
            },
            CommandCapability {
                name: "overview",
                summary: SUMMARY_OVERVIEW,
                invocation: "overview <path>",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["overview.summary", "overview.landmark"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/overview-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "focus",
                summary: SUMMARY_FOCUS,
                invocation: "focus <path> (<object> | --pages <start..end>) [--related]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["focus.summary", "focus.object", "focus.visual_reference"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/semantic-viewport.json"),
                result_root: None,
            },
            CommandCapability {
                name: "peek",
                summary: SUMMARY_PEEK,
                invocation: "peek <path> (--page <page> | --pages <start..end> | --object <id> | --section <index>) [--related]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["peek.summary", "peek.object"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/peek-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "context",
                summary: SUMMARY_CONTEXT,
                invocation: "context <path> (<object> | --find <text>) [--kind <kind>] [--include <classes>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["context", "context.candidate"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/context-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "resolve",
                summary: SUMMARY_RESOLVE,
                invocation: "resolve <path> --text <text> [--kind <kind>] [--pages <start..end>]",
                formats: DOCX_PDF_FORMATS,
                ndjson: true,
                ndjson_events: &["resolve.summary", "resolve.candidate"],
                bounded: true,
                result_schema: Some("https://docsight.dev/schemas/v2/resolve-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "cache",
                summary: SUMMARY_CACHE,
                invocation: "cache <stats|verify|prune|clear> --cache-dir <path>",
                formats: NO_DOCUMENT_FORMATS,
                ndjson: true,
                ndjson_events: &["cache"],
                bounded: false,
                result_schema: Some("https://docsight.dev/schemas/v2/cache-result.json"),
                result_root: None,
            },
            CommandCapability {
                name: "mcp",
                summary: SUMMARY_MCP,
                invocation: "mcp",
                formats: DOCX_PDF_FORMATS,
                ndjson: false,
                ndjson_events: &[],
                bounded: false,
                result_schema: None,
                result_root: None,
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
        let mut item = serde_json::to_value(&result).map_err(output_serialization_error)?;
        let fields = item.as_object_mut().ok_or_else(|| {
            output_serialization_error(serde_json::Error::io(io::Error::other(
                "capabilities result must serialize to an object",
            )))
        })?;
        fields.insert("seq".to_owned(), serde_json::json!(2));
        fields.insert("type".to_owned(), serde_json::json!("capabilities"));
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
    writeln!(
        writer,
        "Errors: {} (stdout carries no bytes on errors)",
        result.error_channel
    )
    .map_err(stdout_error)?;
    for command in result.commands {
        writeln!(writer, "{}: {}", command.name, command.summary).map_err(stdout_error)?;
    }
    Ok(())
}

fn completions(shell: Shell) -> Result<(), DocsightError> {
    let mut command = Cli::command();
    command.set_bin_name("docsight");
    command.build();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    shell
        .try_generate(&command, &mut writer)
        .map_err(stdout_error)
}

fn inspect(
    path: &PathBuf,
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let (result, warnings) = inspect_source(&source, loader)?;

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
        writer.write_warnings(&warnings)?;
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
    if let Some(figures) = result.figures
        && figures > 0
    {
        writeln!(writer, "Figures     {figures}").map_err(stdout_error)?;
    }
    if let Some(comments) = result.comments
        && comments > 0
    {
        writeln!(writer, "Comments    {comments}").map_err(stdout_error)?;
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

pub(crate) fn inspect_source(
    source: &DocumentSource,
    loader: &DocumentLoader<'_>,
) -> Result<(InspectResult, Vec<Diagnostic>), DocsightError> {
    match source.format() {
        DocumentFormat::Docx => {
            let document = loader.load(source)?;
            let paragraphs = document.paragraphs().count() + document.list_items().count();
            let tracked = (document.tracked_changes.insertions > 0
                || document.tracked_changes.deletions > 0)
                .then_some(document.tracked_changes);
            let capability_details = InspectCapabilityDetails::from_document(&document);
            let result = InspectResult {
                format: source.format(),
                size_bytes: source.size_bytes(),
                capabilities: capability_details.available(),
                source_faithful: capability_details.source_faithful(),
                capability_details,
                blocks_by_kind: text_blocks_by_kind(&document),
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
        DocumentFormat::Pdf => inspect_pdf_source(source, loader),
    }
}

fn inspect_pdf_source(
    source: &DocumentSource,
    loader: &DocumentLoader<'_>,
) -> Result<(InspectResult, Vec<Diagnostic>), DocsightError> {
    let unavailable = |pages: Option<u32>, error: DocsightError| {
        let mut diagnostic = error.diagnostic();
        diagnostic.severity = DiagnosticSeverity::Warning;
        let capability_details = InspectCapabilityDetails::unsupported();
        let result = InspectResult {
            format: DocumentFormat::Pdf,
            size_bytes: source.size_bytes(),
            capabilities: capability_details.available(),
            source_faithful: capability_details.source_faithful(),
            capability_details,
            blocks_by_kind: BTreeMap::new(),
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

    let mut page_count = None;
    let loaded = loader.load_with(source, || {
        let pdf = PdfDocument::open_with_password(source, loader.password())?;
        page_count = Some(pdf.page_count());
        pdf.to_document()
    });
    let document = match loaded {
        Ok(document) => document,
        Err(error @ DocsightError::UnsupportedFeature { .. }) => {
            return Ok(unavailable(page_count, error));
        }
        Err(error) => return Err(error),
    };
    let pages = Some(document.pages.len() as u32);
    let warnings = document.warnings.clone();
    let capability_details = InspectCapabilityDetails::from_document(&document);
    let result = InspectResult {
        format: DocumentFormat::Pdf,
        size_bytes: source.size_bytes(),
        capabilities: capability_details.available(),
        source_faithful: capability_details.source_faithful(),
        capability_details,
        blocks_by_kind: text_blocks_by_kind(&document),
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
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let document = loader.load(&source)?;
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
        writer.write_warnings(&document.warnings)?;
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

struct PageCommandArgs<'a> {
    path: &'a PathBuf,
    number: u32,
    loader: &'a DocumentLoader<'a>,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

pub(crate) fn page_spans_and_overlays(
    document: &Document,
    number: u32,
) -> Result<(Vec<PageSpanRecord>, Vec<PageOverlayRecord>), DocsightError> {
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
    let spans: Vec<PageSpanRecord> = document
        .blocks_on_page(number)
        .map(|block| {
            let bbox =
                block
                    .bbox_on_page(number)
                    .ok_or_else(|| DocsightError::MalformedDocument {
                        message: format!(
                            "block {} is indexed on page {} without fragment geometry",
                            block.id, number
                        ),
                    })?;
            let text =
                block
                    .text_on_page(number)?
                    .ok_or_else(|| DocsightError::MalformedDocument {
                        message: format!(
                            "block {} is indexed on page {} without fragment text",
                            block.id, number
                        ),
                    })?;
            Ok(PageSpanRecord {
                id: block.id.clone(),
                text,
                bbox,
                reading_order: block.reading_order,
                confidence: block.confidence,
                source: block.source.path.clone(),
                continued: block.page != Some(number),
            })
        })
        .collect::<Result<_, DocsightError>>()?;
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
    Ok((spans, overlays))
}

fn page_command(args: PageCommandArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let (spans, overlays) = page_spans_and_overlays(&document, args.number)?;
    let target_page = document
        .page(args.number)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {}", args.number),
        })?;
    let page_fidelity = docsight_core::page_fidelity(&document);
    let target_page_number = target_page.number;
    let target_page_width = target_page.width_pt;
    let target_page_height = target_page.height_pt;

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            args.limits.clone(),
            "page".into(),
            source.sha256().to_owned(),
            spans.len() + overlays.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        writer.write_page_begin(args.number)?;
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
        writer.write_page_end(args.number)?;
        writer.write_warnings(&document.warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
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
            args.limits,
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
                    page_fidelity: page_fidelity.clone(),
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
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

fn text_records(document: &Document) -> Vec<TextRecord> {
    document
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
        .filter(|record| !record.text.trim().is_empty())
        .collect()
}

fn text_blocks_by_kind(document: &Document) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for record in text_records(document) {
        *counts.entry(record.kind).or_insert(0) += 1;
    }
    counts
}

fn document_text(
    path: &PathBuf,
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let document = loader.load(&source)?;
    let blocks = text_records(&document);

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
        writer.write_warnings(&document.warnings)?;
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
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let document = loader.load(&source)?;
    let page_fidelity = docsight_core::page_fidelity(&document);
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
        writer.write_warnings(&document.warnings)?;
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
                serde_json::to_value(TablesResult {
                    tables: tbls,
                    page_fidelity: page_fidelity.clone(),
                })
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
    loader: &'a DocumentLoader<'a>,
    object: &'a str,
    format: TableFormat,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn table(args: TableCommandArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
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
    password: &'a [u8],
    target: RenderTarget,
    dpi: u16,
    out: &'a Path,
    trace: Option<&'a Path>,
    command: &'a str,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
    max_document_bytes: u64,
}

fn render(args: RenderCommandArgs<'_>) -> Result<(), DocsightError> {
    let source = DocumentSource::open_with_limit(args.path, args.max_document_bytes)?;
    let request = RenderRequest {
        target: args.target,
        dpi: args.dpi,
    };
    let rendered = render_document_with_password(&source, &request, args.password)?;
    let trace = args
        .trace
        .map(|path| {
            let trace = record_trace_with_password(&source, &request, args.password)?;
            if trace.manifest.raster.sha256 != digest_bytes(rendered.png()) {
                return Err(DocsightError::VerificationFailed {
                    message: "rendered PNG does not match its deterministic trace".to_owned(),
                });
            }
            let trace_bytes = trace.to_bytes()?;
            let output_path = path
                .to_str()
                .ok_or_else(|| DocsightError::InvalidArgument {
                    message: "trace path must be valid UTF-8 for agent output".to_owned(),
                })?
                .to_owned();
            let output = TraceOutput {
                output_path,
                output_sha256: digest_bytes(&trace_bytes),
                output_bytes: u64::try_from(trace_bytes.len()).map_err(|_| {
                    DocsightError::ResourceLimit {
                        resource: "trace artifact bytes".to_owned(),
                        limit: u64::MAX,
                    }
                })?,
                trace_schema: trace.manifest.schema.clone(),
                target: trace.manifest.target.clone(),
                display_list_sha256: trace.manifest.display_list.sha256.clone(),
                raster_sha256: trace.manifest.raster.sha256.clone(),
                decision_coverage: trace.manifest.decision_coverage.clone(),
                resource_count: trace.manifest.resources.len(),
                glyph_run_count: trace.manifest.glyph_runs.len(),
                table_sizing_decision_count: trace.manifest.table_sizing.len(),
                pagination_decision_count: trace.manifest.pagination.len(),
                display_operation_count: trace.manifest.display_list.operations.len(),
            };
            Ok((path, trace_bytes, output))
        })
        .transpose()?;
    rendered.write(args.out)?;
    if let Some((path, bytes, _)) = &trace {
        docsight_core::write_all(path, bytes)?;
    }
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
        #[serde(skip_serializing_if = "Option::is_none")]
        trace: Option<TraceOutput>,
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
        trace: trace.map(|(_, _, output)| output),
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

#[derive(Serialize)]
struct TraceOutput {
    output_path: String,
    output_sha256: String,
    output_bytes: u64,
    trace_schema: String,
    target: TraceTarget,
    display_list_sha256: String,
    raster_sha256: String,
    decision_coverage: TraceDecisionCoverage,
    resource_count: usize,
    glyph_run_count: usize,
    table_sizing_decision_count: usize,
    pagination_decision_count: usize,
    display_operation_count: usize,
}

struct BundleArgs<'a> {
    path: &'a Path,
    password: &'a [u8],
    page: Option<u32>,
    bbox: Option<Rect>,
    object: Option<&'a str>,
    dpi: u16,
    include_crop: bool,
    out: &'a Path,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
    max_document_bytes: u64,
}

fn bundle(args: BundleArgs<'_>) -> Result<(), DocsightError> {
    let target = match (args.page, args.bbox, args.object) {
        (Some(page), Some(bbox), None) => RenderTarget::Region { page, bbox },
        (Some(page), None, None) => RenderTarget::Page { page },
        (None, None, Some(id)) => RenderTarget::Object { id: id.to_owned() },
        (None, None, None) => RenderTarget::Page { page: 1 },
        (None, Some(_), None) => {
            return Err(DocsightError::InvalidArgument {
                message: "--bbox requires --page for bundle".to_owned(),
            });
        }
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "cannot combine --object with --page or --bbox for bundle".to_owned(),
            });
        }
    };
    let source = DocumentSource::open_with_limit(args.path, args.max_document_bytes)?;
    let request = RenderRequest {
        target,
        dpi: args.dpi,
    };
    let proof =
        create_proof_bundle_with_password(&source, &request, args.include_crop, args.password)?;
    let written = proof.write(args.out)?;
    let output_path = args
        .out
        .to_str()
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "bundle path must be valid UTF-8 for agent output".to_owned(),
        })?
        .to_owned();
    #[derive(Serialize)]
    struct BundleResult {
        output_path: String,
        output_sha256: String,
        output_bytes: u64,
        target: TraceTarget,
        evidence_count: usize,
        crop_included: bool,
        trace_display_list_sha256: String,
        trace_raster_sha256: String,
    }
    let result = BundleResult {
        output_path,
        output_sha256: written.sha256,
        output_bytes: written.bytes,
        target: proof.manifest.trace.target.clone(),
        evidence_count: proof.manifest.evidence.len(),
        crop_included: proof.manifest.crop.is_some(),
        trace_display_list_sha256: proof.manifest.trace.display_list.sha256.clone(),
        trace_raster_sha256: proof.manifest.trace.raster.sha256.clone(),
    };
    if args.ndjson {
        return write_single_ndjson(
            &source,
            "bundle",
            "bundle",
            &result,
            &proof.manifest.trace.warnings,
            args.limits,
        );
    }
    if args.json {
        return write_single_json(
            &source,
            &result,
            proof.manifest.trace.warnings.clone(),
            args.limits,
        );
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Proof bundle written to {} ({})",
        args.out.display(),
        result.output_sha256
    )
    .map_err(stdout_error)?;
    writeln!(writer, "  Evidence records: {}", result.evidence_count).map_err(stdout_error)?;
    writeln!(writer, "  Crop included:    {}", result.crop_included).map_err(stdout_error)?;
    emit_warnings(&proof.manifest.trace.warnings, args.quiet, args.json_errors)
}

struct ReplayArgs<'a> {
    trace: &'a Path,
    password: &'a [u8],
    verify: bool,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn replay(args: ReplayArgs<'_>) -> Result<(), DocsightError> {
    if !args.verify {
        return Err(DocsightError::InvalidArgument {
            message: "replay requires --verify".to_owned(),
        });
    }
    let trace = read_trace(args.trace)?;
    let source = trace.document_source()?;
    let verification = verify_trace_with_password(&trace, args.password)?;
    #[derive(Serialize)]
    struct ReplayResult {
        trace_path: String,
        trace_schema: String,
        target: TraceTarget,
        decision_coverage: TraceDecisionCoverage,
        resource_count: usize,
        glyph_run_count: usize,
        table_sizing_decision_count: usize,
        pagination_decision_count: usize,
        display_operation_count: usize,
        verification: docsight_render::trace::ReplayVerification,
    }
    let trace_path = args
        .trace
        .to_str()
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "trace path must be valid UTF-8 for agent output".to_owned(),
        })?
        .to_owned();
    let result = ReplayResult {
        trace_path,
        trace_schema: trace.manifest.schema.clone(),
        target: trace.manifest.target.clone(),
        decision_coverage: trace.manifest.decision_coverage.clone(),
        resource_count: trace.manifest.resources.len(),
        glyph_run_count: trace.manifest.glyph_runs.len(),
        table_sizing_decision_count: trace.manifest.table_sizing.len(),
        pagination_decision_count: trace.manifest.pagination.len(),
        display_operation_count: trace.manifest.display_list.operations.len(),
        verification,
    };
    if args.ndjson {
        return write_single_ndjson(
            &source,
            "replay",
            "replay",
            &result,
            &trace.manifest.warnings,
            args.limits,
        );
    }
    if args.json {
        return write_single_json(
            &source,
            &result,
            trace.manifest.warnings.clone(),
            args.limits,
        );
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Trace replay verified: {}",
        result.verification.trace_sha256
    )
    .map_err(stdout_error)?;
    emit_warnings(&trace.manifest.warnings, args.quiet, args.json_errors)
}

struct VerifyArgs<'a> {
    bundle: &'a Path,
    password: &'a [u8],
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn verify(args: VerifyArgs<'_>) -> Result<(), DocsightError> {
    let bundle = read_proof_bundle(args.bundle)?;
    let source = bundle.document_source()?;
    let verification = verify_proof_bundle_with_password(&bundle, args.password)?;
    #[derive(Serialize)]
    struct VerifyResult {
        bundle_name: String,
        verification: docsight_render::trace::ProofVerification,
    }
    let bundle_name = args
        .bundle
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DocsightError::InvalidArgument {
            message: "bundle path must end in a valid UTF-8 file name for agent output".to_owned(),
        })?
        .to_owned();
    let result = VerifyResult {
        bundle_name,
        verification,
    };
    if args.ndjson {
        return write_single_ndjson(
            &source,
            "verify",
            "verify",
            &result,
            &bundle.manifest.trace.warnings,
            args.limits,
        );
    }
    if args.json {
        return write_single_json(
            &source,
            &result,
            bundle.manifest.trace.warnings.clone(),
            args.limits,
        );
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Proof bundle verified: {}",
        result.verification.bundle_sha256
    )
    .map_err(stdout_error)?;
    emit_warnings(
        &bundle.manifest.trace.warnings,
        args.quiet,
        args.json_errors,
    )
}

fn images(
    path: &PathBuf,
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let document = loader.load(&source)?;
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
        writer.write_warnings(&document.warnings)?;
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
    loader: &DocumentLoader<'_>,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = loader.open_source(path)?;
    let document = loader.load(&source)?;
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
        writer.write_warnings(&document.warnings)?;
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
    let warnings = docsight_agent::consolidate_warnings(warnings);
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
        let envelope = adaptive_agent_envelope(
            source,
            val.clone(),
            selected_warnings.clone(),
            limits_record,
            limits,
        )?;
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
    writer.write_warnings(warnings)?;
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
    for warning in &docsight_agent::consolidate_warnings(warnings.to_vec()) {
        if json_errors {
            let val = serde_json::json!({
                "schema": docsight_agent::AGENT_SCHEMA,
                "warning": warning,
            });
            let line = serde_json::to_string(&val).map_err(output_serialization_error)?;
            writeln!(writer, "{line}").map_err(stderr_error)?;
        } else if let Some(occurrences) = warning.occurrences {
            writeln!(
                writer,
                "{}: {} ({occurrences} occurrences)",
                warning.code, warning.message
            )
            .map_err(stderr_error)?;
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

fn parse_byte_budget(raw: &str) -> Result<usize, String> {
    if raw.is_empty() || raw.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(
            "byte budget must be a positive integer with optional b, kb or mb suffix".to_owned(),
        );
    }
    let digit_count = raw.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return Err("byte budget must start with a positive integer".to_owned());
    }
    let (digits, suffix) = raw.split_at(digit_count);
    let value = digits
        .parse::<usize>()
        .map_err(|_| "byte budget integer is out of range".to_owned())?;
    if value == 0 {
        return Err("byte budget must be greater than zero".to_owned());
    }
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1_usize,
        "kb" => 1024_usize,
        "mb" => 1024_usize
            .checked_mul(1024)
            .ok_or_else(|| "byte budget multiplier overflowed".to_owned())?,
        _ => return Err("byte budget suffix must be b, kb or mb".to_owned()),
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| "byte budget is out of range".to_owned())
}

pub(crate) fn parse_bbox(raw: &str) -> Result<Rect, String> {
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

pub(crate) fn parse_page_range(raw: &str) -> Result<PageRange, String> {
    let (start, end) = match raw.split_once("..") {
        Some((start, end)) => (start, end),
        None => (raw, raw),
    };
    let start = start
        .parse::<u32>()
        .map_err(|_| "page range start must be a positive integer".to_owned())?;
    let end = end
        .parse::<u32>()
        .map_err(|_| "page range end must be a positive integer".to_owned())?;
    PageRange::new(start, end).map_err(|error| error.to_string())
}

fn continuation_scope(command: &str, parameter: &str) -> String {
    let digest = digest_bytes(parameter.as_bytes());
    format!("{command}_{}", &digest[..16])
}

struct FindArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    pattern: &'a str,
    regex: bool,
    ignore_case: bool,
    kinds: &'a [FindKindArg],
    pages: Option<PageRange>,
    bbox: Option<Rect>,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

#[derive(Clone, Debug, Serialize)]
struct FindNdjsonSummary {
    pattern: String,
    mode: FindMode,
    ignore_case: bool,
    total_matches: usize,
    searched_objects: usize,
    geometry_unavailable_matches: usize,
}

fn find_command(args: FindArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let request = FindRequest {
        pattern: args.pattern.to_owned(),
        mode: if args.regex {
            FindMode::Regex
        } else {
            FindMode::Literal
        },
        ignore_case: args.ignore_case,
        kinds: args.kinds.iter().map(|kind| kind.to_find_kind()).collect(),
        pages: args.pages.map(|range| (range.start, range.end)),
        region: args.bbox,
    };
    let result = find_occurrences(&document, &request)?;
    let warnings = document.warnings;
    let scope = continuation_scope(
        "find",
        &format!(
            "{}|{:?}|{}|{:?}|{:?}|{:?}",
            request.pattern,
            request.mode,
            request.ignore_case,
            request.kinds,
            request.pages,
            request.region
        ),
    );
    let limits = bounded_machine_limits(args.limits, DEFAULT_QUERY_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.matches.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(FindNdjsonSummary {
            pattern: result.pattern.clone(),
            mode: result.mode,
            ignore_case: result.ignore_case,
            total_matches: result.total_matches,
            searched_objects: result.searched_objects,
            geometry_unavailable_matches: result.geometry_unavailable_matches,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("find.summary", &summary)? {
            writer.write_warnings(&warnings)?;
            writer.finish()?;
            return Ok(());
        }
        for (index, item) in result.matches.iter().enumerate() {
            if index + 1 < offset {
                continue;
            }
            let item = serde_json::to_value(item).map_err(output_serialization_error)?;
            if !writer.write_item("find.match", &item)? {
                break;
            }
        }
        writer.write_warnings(&warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let FindResult {
            pattern,
            mode,
            ignore_case,
            total_matches,
            searched_objects,
            geometry_unavailable_matches,
            matches,
        } = result;
        let envelope =
            apply_bounded_collection(&matches, &limits, &scope, &source, warnings, |returned| {
                serde_json::to_value(FindResult {
                    pattern: pattern.clone(),
                    mode,
                    ignore_case,
                    total_matches,
                    searched_objects,
                    geometry_unavailable_matches,
                    matches: returned,
                })
                .map_err(output_serialization_error)
            })?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Find {:?} ({:?}{}): {} matches in {} objects",
        result.pattern,
        result.mode,
        if result.ignore_case {
            ", ignore case"
        } else {
            ""
        },
        result.total_matches,
        result.searched_objects
    )
    .map_err(stdout_error)?;
    for found in &result.matches {
        let page = found
            .page
            .map(|page| page.to_string())
            .unwrap_or_else(|| "unplaced".to_owned());
        writeln!(
            writer,
            "[{}] {:?} page {}: ...{}[{}]{}...",
            found.object_id,
            found.kind,
            page,
            found.context_before,
            found.matched.text,
            found.context_after
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&warnings, args.quiet, args.json_errors)
}

struct QueryArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    expression: &'a str,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn query(args: QueryArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let execution = execute_spatial_query(&document, args.expression)?;
    let mut warnings = document.warnings;
    warnings.extend(execution.warnings);
    let result = execution.result;
    let scope = continuation_scope("query", args.expression);
    let limits = bounded_machine_limits(args.limits, DEFAULT_QUERY_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.matches.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(QueryNdjsonSummary {
            query: result.query.clone(),
            total_matches: result.total_matches,
            geometry_unavailable_objects: result.geometry_unavailable_objects,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("query.summary", &summary)? {
            writer.write_warnings(&warnings)?;
            writer.finish()?;
            return Ok(());
        }
        {
            for (index, item) in result.matches.iter().enumerate() {
                if index + 1 < offset {
                    continue;
                }
                let item = serde_json::to_value(item).map_err(output_serialization_error)?;
                if !writer.write_item("query.match", &item)? {
                    break;
                }
            }
        }
        writer.write_warnings(&warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let total_matches = result.total_matches;
        let geometry_unavailable_objects = result.geometry_unavailable_objects;
        let query = result.query.clone();
        let envelope = apply_bounded_collection(
            &result.matches,
            &limits,
            &scope,
            &source,
            warnings,
            |matches| {
                serde_json::to_value(SpatialQueryResult {
                    query: query.clone(),
                    total_matches,
                    geometry_unavailable_objects,
                    matches,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Query: {}", result.query).map_err(stdout_error)?;
    writeln!(writer, "Matches: {}", result.total_matches).map_err(stdout_error)?;
    for item in &result.matches {
        let page = item
            .object
            .page
            .map(|page| page.to_string())
            .unwrap_or_else(|| "unplaced".to_owned());
        let relation = item
            .relation
            .as_ref()
            .map(|relation| {
                format!(
                    " {:?} {} ({:.3} pt)",
                    relation.kind, relation.anchor, relation.distance_pt
                )
            })
            .unwrap_or_default();
        writeln!(
            writer,
            "[{}] {:?} page {}{}",
            item.object.id, item.object.kind, page, relation
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&warnings, args.quiet, args.json_errors)
}

struct OverviewArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn overview(args: OverviewArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let result = document_overview(&document)?;
    let limits = bounded_machine_limits(args.limits, DEFAULT_VIEWPORT_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "overview".to_owned(),
            source.sha256().to_owned(),
            1 + result.landmarks.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(OverviewNdjsonSummary {
            format: result.format,
            page_count: result.page_count,
            counts: result.counts.clone(),
            total_landmarks: result.total_landmarks,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("overview.summary", &summary)? {
            writer.write_warnings(&document.warnings)?;
            writer.finish()?;
            return Ok(());
        }
        {
            for (index, landmark) in result.landmarks.iter().enumerate() {
                if index + 1 < offset {
                    continue;
                }
                let item = serde_json::to_value(landmark).map_err(output_serialization_error)?;
                if !writer.write_item("overview.landmark", &item)? {
                    break;
                }
            }
        }
        writer.write_warnings(&document.warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let format = result.format;
        let page_count = result.page_count;
        let counts = result.counts.clone();
        let total_landmarks = result.total_landmarks;
        let envelope = apply_bounded_collection(
            &result.landmarks,
            &limits,
            "overview",
            &source,
            document.warnings,
            |landmarks| {
                serde_json::to_value(docsight_search::OverviewResult {
                    format,
                    page_count,
                    counts: counts.clone(),
                    total_landmarks,
                    landmarks,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{} pages", result.page_count).map_err(stdout_error)?;
    writeln!(writer, "{} landmarks", result.total_landmarks).map_err(stdout_error)?;
    for landmark in &result.landmarks {
        let page = landmark
            .page
            .map(|page| page.to_string())
            .unwrap_or_else(|| "unplaced".to_owned());
        writeln!(
            writer,
            "[{}] {:?} page {}",
            landmark.id, landmark.kind, page
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

struct FocusArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    target: Option<&'a str>,
    pages: Option<PageRange>,
    related: bool,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn focus(args: FocusArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let (result, scope) = match (args.target, args.pages) {
        (Some(target), None) => (
            focus_object(&document, target, args.related)?,
            continuation_scope("focus", target),
        ),
        (None, Some(pages)) if !args.related => (
            focus_pages(&document, pages)?,
            continuation_scope("focus", &format!("{}..{}", pages.start, pages.end)),
        ),
        (None, Some(_)) => {
            return Err(DocsightError::InvalidArgument {
                message: "--related requires an object target, not --pages".to_owned(),
            });
        }
        (Some(_), Some(_)) => {
            return Err(DocsightError::InvalidArgument {
                message: "focus accepts either an object target or --pages, not both".to_owned(),
            });
        }
        (None, None) => {
            return Err(DocsightError::InvalidArgument {
                message: "focus requires an object target or --pages <start..end>".to_owned(),
            });
        }
    };
    let limits = bounded_machine_limits(args.limits, DEFAULT_VIEWPORT_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.objects.len() + result.visual_references.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(FocusNdjsonSummary {
            target: result.target.clone(),
            scope_pages: result.scope_pages.clone(),
            total_objects: result.total_objects,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("focus.summary", &summary)? {
            writer.write_warnings(&document.warnings)?;
            writer.finish()?;
            return Ok(());
        }
        {
            for (index, item) in result.objects.iter().enumerate() {
                if index + 1 < offset {
                    continue;
                }
                let item = serde_json::to_value(item).map_err(output_serialization_error)?;
                if !writer.write_item("focus.object", &item)? {
                    break;
                }
            }
            let visual_start = 1 + result.objects.len();
            for (index, item) in result.visual_references.iter().enumerate() {
                if visual_start + index < offset {
                    continue;
                }
                let item = serde_json::to_value(item).map_err(output_serialization_error)?;
                if !writer.write_item("focus.visual_reference", &item)? {
                    break;
                }
            }
        }
        writer.write_warnings(&document.warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let target = result.target.clone();
        let scope_pages = result.scope_pages.clone();
        let total_objects = result.total_objects;
        let visual_references = result.visual_references.clone();
        let envelope = apply_bounded_collection(
            &result.objects,
            &limits,
            &scope,
            &source,
            document.warnings,
            |objects| {
                serde_json::to_value(SemanticViewport {
                    target: target.clone(),
                    scope_pages: scope_pages.clone(),
                    total_objects,
                    objects,
                    visual_references: visual_references.clone(),
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Focus: {:?}", result.target).map_err(stdout_error)?;
    writeln!(writer, "Pages: {:?}", result.scope_pages).map_err(stdout_error)?;
    for item in &result.objects {
        let roles = item
            .relationships
            .iter()
            .map(|relationship| format!("{:?}", relationship.role))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            writer,
            "[{}] {:?}: {}",
            item.object.id, item.object.kind, roles
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

struct PeekArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    page: Option<u32>,
    pages: Option<PageRange>,
    object: Option<&'a str>,
    section: Option<u32>,
    related: bool,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn peek(args: PeekArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let target_count = [
        args.page.is_some(),
        args.pages.is_some(),
        args.object.is_some(),
        args.section.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if target_count != 1 {
        return Err(DocsightError::InvalidArgument {
            message: "peek requires exactly one of --page, --pages, --object or --section"
                .to_owned(),
        });
    }
    if args.related && args.object.is_none() {
        return Err(DocsightError::InvalidArgument {
            message: "peek --related requires --object".to_owned(),
        });
    }
    let result = match (args.page, args.pages, args.object, args.section) {
        (Some(page), None, None, None) => peek_pages(&document, PageRange::new(page, page)?)?,
        (None, Some(pages), None, None) => peek_pages(&document, pages)?,
        (None, None, Some(object), None) => peek_object(&document, object, args.related)?,
        (None, None, None, Some(section)) => peek_section(&document, section)?,
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "peek target combination is invalid".to_owned(),
            });
        }
    };
    let scope = continuation_scope(
        "peek",
        &serde_json::to_string(&result.target).map_err(output_serialization_error)?,
    );
    let limits = bounded_machine_limits(args.limits, DEFAULT_PEEK_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.objects.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(PeekNdjsonSummary {
            target: result.target.clone(),
            scope_pages: result.scope_pages.clone(),
            total_objects: result.total_objects,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("peek.summary", &summary)? {
            writer.write_warnings(&document.warnings)?;
            writer.finish()?;
            return Ok(());
        }
        for (index, object) in result.objects.iter().enumerate() {
            if index + 1 < offset {
                continue;
            }
            let value = serde_json::to_value(object).map_err(output_serialization_error)?;
            if !writer.write_item("peek.object", &value)? {
                break;
            }
        }
        writer.write_warnings(&document.warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let target = result.target.clone();
        let scope_pages = result.scope_pages.clone();
        let total_objects = result.total_objects;
        let envelope = apply_bounded_collection(
            &result.objects,
            &limits,
            &scope,
            &source,
            document.warnings,
            |objects| {
                serde_json::to_value(PeekResult {
                    target: target.clone(),
                    scope_pages: scope_pages.clone(),
                    total_objects,
                    objects,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Peek: {:?}", result.target).map_err(stdout_error)?;
    writeln!(writer, "Objects: {}", result.total_objects).map_err(stdout_error)?;
    for item in &result.objects {
        let roles = item
            .relationships
            .iter()
            .map(|relationship| format!("{:?}", relationship.role))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            writer,
            "[{}] {:?} page {:?}: {}",
            item.object.id, item.object.kind, item.object.page, roles
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

struct ResolveArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    text: &'a str,
    kind: Option<SemanticKind>,
    pages: Option<PageRange>,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn resolve(args: ResolveArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let result = resolve_descriptor(&document, args.text, args.kind, args.pages)?;
    let query_scope = serde_json::to_string(&result.query).map_err(output_serialization_error)?;
    let scope = continuation_scope("resolve", &query_scope);
    let limits = bounded_machine_limits(args.limits, DEFAULT_RESOLVE_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.candidates.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(ResolveNdjsonSummary {
            query: result.query.clone(),
            status: result.status,
            total_candidates: result.total_candidates,
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("resolve.summary", &summary)? {
            writer.write_warnings(&document.warnings)?;
            writer.finish()?;
            return Ok(());
        }
        for (index, candidate) in result.candidates.iter().enumerate() {
            if index + 1 < offset {
                continue;
            }
            let value = serde_json::to_value(candidate).map_err(output_serialization_error)?;
            if !writer.write_item("resolve.candidate", &value)? {
                break;
            }
        }
        writer.write_warnings(&document.warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let query = result.query.clone();
        let status = result.status;
        let total_candidates = result.total_candidates;
        let envelope = apply_bounded_collection(
            &result.candidates,
            &limits,
            &scope,
            &source,
            document.warnings,
            |candidates| {
                serde_json::to_value(ResolveResult {
                    query: query.clone(),
                    status,
                    total_candidates,
                    candidates,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Resolve: {:?}", result.status).map_err(stdout_error)?;
    for candidate in &result.candidates {
        writeln!(
            writer,
            "[{:.6}] [{}] {:?} page {:?}",
            candidate.score, candidate.object.id, candidate.object.kind, candidate.object.page
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings, args.quiet, args.json_errors)
}

struct ContextArgs<'a> {
    path: &'a Path,
    loader: &'a DocumentLoader<'a>,
    object: Option<&'a str>,
    find: Option<&'a str>,
    kind: Option<SemanticKind>,
    include: &'a [ContextInclude],
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn context(args: ContextArgs<'_>) -> Result<(), DocsightError> {
    let include = args.include.iter().copied().collect::<BTreeSet<_>>();
    if include.len() != args.include.len() {
        return Err(DocsightError::InvalidArgument {
            message: "context --include values must not be duplicated".to_owned(),
        });
    }
    let source = args.loader.open_source(args.path)?;
    let document = args.loader.load(&source)?;
    let mut warnings = document.warnings.clone();
    let (status, selection, total_candidates, candidates, target) = match (args.object, args.find) {
        (Some(object), None) if args.kind.is_none() => {
            context_neighborhood(&document, object, false)?;
            let object_id = ObjectId::from_raw(object);
            let reason = ResolveReason {
                code: docsight_search::ResolveReasonCode::ExplicitObjectId,
                score: 1.0,
                weight: 1.0,
                contribution: 1.0,
                evidence: Some(object.to_owned()),
            };
            (
                ResolveStatus::Resolved,
                ContextSelection {
                    mode: ContextSelectionMode::ExplicitObject,
                    descriptor: None,
                    chosen_object: Some(object_id.clone()),
                    matched_range: None,
                    reasons: vec![reason],
                },
                0,
                Vec::new(),
                Some(object_id),
            )
        }
        (Some(_), None) => {
            return Err(DocsightError::InvalidArgument {
                message: "context --kind is valid only with --find".to_owned(),
            });
        }
        (None, Some(find)) => {
            let resolved = resolve_descriptor(&document, find, args.kind, None)?;
            let chosen = (resolved.status == ResolveStatus::Resolved)
                .then(|| resolved.candidates.first())
                .flatten();
            let selection = ContextSelection {
                mode: ContextSelectionMode::Find,
                descriptor: Some(find.to_owned()),
                chosen_object: chosen.map(|candidate| candidate.object.id.clone()),
                matched_range: chosen.and_then(|candidate| candidate.matched_range.clone()),
                reasons: chosen
                    .map(|candidate| candidate.reasons.clone())
                    .unwrap_or_default(),
            };
            let target = chosen.map(|candidate| candidate.object.id.clone());
            let total_candidates = resolved.total_candidates;
            let candidates = if resolved.status == ResolveStatus::Resolved {
                Vec::new()
            } else {
                resolved.candidates
            };
            (
                resolved.status,
                selection,
                total_candidates,
                candidates,
                target,
            )
        }
        (Some(_), Some(_)) => {
            return Err(DocsightError::InvalidArgument {
                message: "context accepts either an object target or --find, not both".to_owned(),
            });
        }
        (None, None) => {
            return Err(DocsightError::InvalidArgument {
                message: "context requires an object target or --find".to_owned(),
            });
        }
    };
    let package = target
        .as_ref()
        .map(|target| build_context_package(&document, &source, target, &include, &mut warnings))
        .transpose()?;
    let result = ContextResult {
        status,
        selection,
        context: package,
        total_candidates,
        candidates,
    };
    let scope_input = serde_json::to_string(&serde_json::json!({
        "object": args.object,
        "find": args.find,
        "kind": args.kind,
        "include": args.include
    }))
    .map_err(output_serialization_error)?;
    let scope = continuation_scope("context", &scope_input);
    let limits = bounded_machine_limits(args.limits, DEFAULT_RESOLVE_ITEMS);

    if args.ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            scope,
            source.sha256().to_owned(),
            1 + result.candidates.len(),
        )?;
        writer.write_meta(&(&source).into())?;
        let summary = serde_json::to_value(ContextResult {
            status: result.status,
            selection: result.selection.clone(),
            context: result.context.clone(),
            total_candidates: result.total_candidates,
            candidates: Vec::new(),
        })
        .map_err(output_serialization_error)?;
        let offset = writer.continuation_offset();
        if offset == 0 && !writer.write_item("context", &summary)? {
            writer.write_warnings(&warnings)?;
            writer.finish()?;
            return Ok(());
        }
        for (index, candidate) in result.candidates.iter().enumerate() {
            if index + 1 < offset {
                continue;
            }
            let value = serde_json::to_value(candidate).map_err(output_serialization_error)?;
            if !writer.write_item("context.candidate", &value)? {
                break;
            }
        }
        writer.write_warnings(&warnings)?;
        writer.finish()?;
        return Ok(());
    }

    if args.json {
        let status = result.status;
        let selection = result.selection.clone();
        let package = result.context.clone();
        let total_candidates = result.total_candidates;
        let envelope = apply_bounded_collection(
            &result.candidates,
            &limits,
            &scope,
            &source,
            warnings,
            |candidates| {
                serde_json::to_value(ContextResult {
                    status,
                    selection: selection.clone(),
                    context: package.clone(),
                    total_candidates,
                    candidates,
                })
                .map_err(output_serialization_error)
            },
        )?;
        return write_envelope(&envelope);
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "Context: {:?}", result.status).map_err(stdout_error)?;
    if let Some(chosen) = &result.selection.chosen_object {
        writeln!(writer, "Object: {chosen}").map_err(stdout_error)?;
    }
    if let Some(package) = &result.context {
        writeln!(writer, "Page: {:?}", package.target.page).map_err(stdout_error)?;
        writeln!(writer, "Kind: {:?}", package.target.kind).map_err(stdout_error)?;
    }
    emit_warnings(&warnings, args.quiet, args.json_errors)
}

fn build_context_package(
    document: &Document,
    source: &DocumentSource,
    target_id: &ObjectId,
    include: &BTreeSet<ContextInclude>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<ContextPackage, DocsightError> {
    let neighborhood = context_neighborhood(
        document,
        target_id.as_str(),
        include.contains(&ContextInclude::Related),
    )?;
    let target = neighborhood
        .objects
        .iter()
        .find(|entry| {
            entry.object.id == *target_id
                && entry
                    .relationships
                    .iter()
                    .any(|relationship| relationship.role == ViewportRole::Target)
        })
        .map(|entry| entry.object.clone())
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: target_id.to_string(),
        })?;
    let resolved = document.resolve_object(target_id.as_str()).ok_or_else(|| {
        DocsightError::ObjectNotFound {
            object: target_id.to_string(),
        }
    })?;
    let page = target
        .page
        .and_then(|number| document.page(number))
        .map(|page| ContextPage {
            number: page.number,
            width_pt: page.width_pt,
            height_pt: page.height_pt,
        });
    let target_section = if document.format == DocumentFormat::Docx {
        match resolved.anchor_block() {
            Some(block) => document.section_for_block(&block.id)?,
            None => target
                .page
                .and_then(|number| document.page(number))
                .and_then(|page| page.section_index)
                .and_then(|index| {
                    document
                        .sections
                        .iter()
                        .find(|section| section.section_index == index)
                }),
        }
    } else {
        None
    };
    let (section, section_status, section_reason) = match document.format {
        DocumentFormat::Pdf => (
            None,
            ContextSectionStatus::NotApplicable,
            Some("PDF documents do not have DOCX sections".to_owned()),
        ),
        DocumentFormat::Docx if let Some(section) = target_section => (
            Some(ContextSection {
                id: section.id.clone(),
                index: section.section_index,
            }),
            ContextSectionStatus::Exact,
            None,
        ),
        DocumentFormat::Docx => {
            warnings.push(Diagnostic {
                code: "CONTEXT_SECTION_UNAVAILABLE".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: "the target page cannot be assigned to one canonical DOCX section"
                    .to_owned(),
                effect: "context omits the containing section".to_owned(),
                object: Some(target_id.clone()),
                page: target.page,
                occurrences: None,
            });
            (
                None,
                ContextSectionStatus::Unavailable,
                Some("canonical page-to-section mapping is unavailable".to_owned()),
            )
        }
    };
    let content = if include.contains(&ContextInclude::Content) {
        Some(context_content(&resolved)?)
    } else {
        None
    };
    let provenance = include.contains(&ContextInclude::Provenance).then(|| {
        let span = resolved.source();
        ContextProvenance {
            source_path: span.path.clone(),
            source_offset: span.offset,
            source_length: span.length,
            confidence: resolved.confidence(),
        }
    });
    let fidelity = if include.contains(&ContextInclude::Fidelity) {
        match resolved.anchor_block() {
            Some(_) => {
                let glyph_coverage = document_glyph_coverage(document, source);
                let evidence = compute_evidence(document, source, target_id, None, glyph_coverage)?;
                Some(ContextFidelity {
                    available: true,
                    text: Some(evidence.fidelity.text),
                    structure: Some(evidence.fidelity.structure),
                    geometry: Some(evidence.fidelity.geometry),
                    visual: Some(evidence.fidelity.visual),
                    reasons: evidence.fidelity.reasons,
                })
            }
            None => {
                warnings.push(Diagnostic {
                    code: "CONTEXT_FIDELITY_UNAVAILABLE".to_owned(),
                    severity: DiagnosticSeverity::Warning,
                    message: "object-level fidelity is unavailable for an object outside the reading flow".to_owned(),
                    effect: "context exposes provenance and geometry but no fidelity scores"
                        .to_owned(),
                    object: Some(target_id.clone()),
                    page: target.page,
                    occurrences: None,
                });
                Some(ContextFidelity {
                    available: false,
                    text: None,
                    structure: None,
                    geometry: None,
                    visual: None,
                    reasons: vec!["CONTEXT_FIDELITY_UNAVAILABLE".to_owned()],
                })
            }
        }
    } else {
        None
    };
    let geometry = include
        .contains(&ContextInclude::Geometry)
        .then(|| ContextGeometry {
            available: target.page.is_some() && target.bbox.is_some(),
            coordinate_system: "page-local points at 1/72 inch with top-left origin",
            page: target.page,
            bbox: target.bbox,
            z_index: target.z_index,
            reading_order: target.reading_order,
        });
    let heading = include
        .contains(&ContextInclude::Heading)
        .then(|| first_related(&neighborhood.objects, &[ViewportRole::ParentHeading]))
        .flatten();
    let neighbors = include.contains(&ContextInclude::Neighbors).then(|| {
        related_objects(
            &neighborhood.objects,
            &[ViewportRole::Previous, ViewportRole::Next],
        )
    });
    let related = include.contains(&ContextInclude::Related).then(|| {
        related_objects(
            &neighborhood.objects,
            &[ViewportRole::RelatedCaption, ViewportRole::RelatedNote],
        )
    });
    let container = resolved
        .container()
        .map(|container| ContextContainerObject {
            id: container.id.clone(),
            kind: container.kind,
        });
    Ok(ContextPackage {
        target,
        containers: ContextContainers {
            page,
            section,
            section_status,
            section_reason,
            object: container,
        },
        heading,
        neighbors,
        related,
        content,
        geometry,
        fidelity,
        provenance,
    })
}

fn first_related(
    objects: &[ViewportObject],
    roles: &[ViewportRole],
) -> Option<ContextRelatedObject> {
    related_objects(objects, roles).into_iter().next()
}

fn related_objects(
    objects: &[ViewportObject],
    roles: &[ViewportRole],
) -> Vec<ContextRelatedObject> {
    let mut related = objects
        .iter()
        .flat_map(|entry| {
            entry
                .relationships
                .iter()
                .filter(|relationship| roles.contains(&relationship.role))
                .map(|relationship| ContextRelatedObject {
                    role: relationship.role,
                    confidence: relationship.confidence,
                    provenance: relationship.provenance.clone(),
                    object: entry.object.clone(),
                })
        })
        .collect::<Vec<_>>();
    related.sort_by(|left, right| {
        left.object
            .page
            .unwrap_or(u32::MAX)
            .cmp(&right.object.page.unwrap_or(u32::MAX))
            .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
            .then_with(|| left.object.id.cmp(&right.object.id))
            .then_with(|| format!("{:?}", left.role).cmp(&format!("{:?}", right.role)))
    });
    related
}

struct DiffCommandArgs<'a> {
    before: &'a Path,
    after: &'a Path,
    password_before: &'a [u8],
    password_after: &'a [u8],
    summary: bool,
    json: bool,
    ndjson: bool,
    options: DiffOptions,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
    max_document_bytes: u64,
}

#[derive(Serialize)]
struct DiffNdjsonSummary<'a> {
    #[serde(flatten)]
    summary: &'a DiffSummary,
    before_document: &'a DiffDocumentIdentity,
    after_document: &'a DiffDocumentIdentity,
}

#[derive(Serialize)]
struct DiffNdjsonVisualSummary<'a> {
    authoritative: bool,
    evidence_status: docsight_diff::VisualDiffEvidenceStatus,
    reason_codes: &'a [String],
    dpi: u16,
    threshold: u8,
    pages_before: u32,
    pages_after: u32,
    layout_changed_pages: u32,
    layout_regression_score: f32,
    largest_drift_pt: Option<f32>,
    largest_drift_page: Option<u32>,
    page_diff_count: usize,
}

impl<'a> From<&'a VisualDiff> for DiffNdjsonVisualSummary<'a> {
    fn from(visual: &'a VisualDiff) -> Self {
        Self {
            authoritative: visual.authoritative,
            evidence_status: visual.evidence_status,
            reason_codes: &visual.reason_codes,
            dpi: visual.dpi,
            threshold: visual.threshold,
            pages_before: visual.pages_before,
            pages_after: visual.pages_after,
            layout_changed_pages: visual.layout_changed_pages,
            layout_regression_score: visual.layout_regression_score,
            largest_drift_pt: visual.largest_drift_pt,
            largest_drift_page: visual.largest_drift_page,
            page_diff_count: visual.page_diffs.len(),
        }
    }
}

fn diff(args: DiffCommandArgs<'_>) -> Result<(), DocsightError> {
    let source_before = DocumentSource::open_with_limit(args.before, args.max_document_bytes)?;
    let source_after = DocumentSource::open_with_limit(args.after, args.max_document_bytes)?;
    let diff_result = diff_documents_with_passwords(
        &source_before,
        &source_after,
        &args.options,
        args.password_before,
        args.password_after,
    )?;

    if args.ndjson {
        let stdout = io::stdout();
        let visual_items = match &diff_result.visual {
            Some(visual) => visual.page_diffs.len().checked_add(1).ok_or_else(|| {
                DocsightError::ResourceLimit {
                    resource: "visual diff NDJSON event count".to_owned(),
                    limit: u64::MAX,
                }
            })?,
            None => 0,
        };
        let expected_items = 1_usize
            .checked_add(visual_items)
            .and_then(|count| count.checked_add(diff_result.semantic.lineage.len()))
            .and_then(|count| count.checked_add(diff_result.semantic.records.len()))
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "diff NDJSON event count".to_owned(),
                limit: u64::MAX,
            })?;
        let scope = continuation_scope(
            "diff",
            &format!(
                "before={};after={};visual={};dpi={};threshold={};artifacts={}",
                source_before.sha256(),
                source_after.sha256(),
                args.options.visual,
                args.options.dpi,
                args.options.threshold,
                args.options.out_dir.is_some()
            ),
        );
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            args.limits.clone(),
            scope,
            source_after.sha256().to_owned(),
            expected_items,
        )?;
        writer.write_meta(&(&source_after).into())?;
        let summary_val = serde_json::to_value(DiffNdjsonSummary {
            summary: &diff_result.summary,
            before_document: &diff_result.before_document,
            after_document: &diff_result.after_document,
        })
        .map_err(output_serialization_error)?;
        writer.write_item("diff.summary", &summary_val)?;
        if let Some(visual) = &diff_result.visual {
            let visual_summary = serde_json::to_value(DiffNdjsonVisualSummary::from(visual))
                .map_err(output_serialization_error)?;
            writer.write_item("diff.visual", &visual_summary)?;
            for page in &visual.page_diffs {
                let page_value = serde_json::to_value(page).map_err(output_serialization_error)?;
                if !writer.write_item("diff.visual.page", &page_value)? {
                    break;
                }
            }
        }
        for record in &diff_result.semantic.lineage {
            let record_val = serde_json::to_value(record).map_err(output_serialization_error)?;
            if !writer.write_item("diff.lineage", &record_val)? {
                break;
            }
        }
        for record in &diff_result.semantic.records {
            let record_val = serde_json::to_value(record).map_err(output_serialization_error)?;
            if !writer.write_item("diff.semantic", &record_val)? {
                break;
            }
        }
        writer.write_warnings(&diff_result.warnings)?;
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
        for record in diff_result
            .semantic
            .lineage
            .iter()
            .filter(|record| record.status == docsight_diff::LineageStatus::Ambiguous)
        {
            writeln!(
                writer,
                "  ! ambiguous {} lineage {} with {} candidates",
                record.target_type,
                record.id,
                record.candidates.len()
            )
            .map_err(stdout_error)?;
        }
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
    max_document_bytes: u64,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open_with_limit(path, max_document_bytes)?;
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
    loader: &'a DocumentLoader<'a>,
    object: &'a str,
    render_dpi: u16,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn evidence(args: EvidenceArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let doc = args.loader.load(&source)?;
    let obj_id = ObjectId::from_raw(args.object);
    let mut extra_warnings = Vec::new();

    let render_fingerprint = {
        let req = RenderRequest {
            target: RenderTarget::Object {
                id: args.object.to_owned(),
            },
            dpi: args.render_dpi,
        };
        match render_document_with_password(&source, &req, args.loader.password()) {
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
                    occurrences: None,
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

pub(crate) fn document_glyph_coverage(doc: &Document, source: &DocumentSource) -> f32 {
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
    loader: &'a DocumentLoader<'a>,
    page: Option<u32>,
    regions: bool,
    json: bool,
    ndjson: bool,
    limits: &'a QueryLimits,
    quiet: bool,
    json_errors: bool,
}

fn coverage(args: CoverageArgs<'_>) -> Result<(), DocsightError> {
    let source = args.loader.open_source(args.path)?;
    let doc = args.loader.load(&source)?;
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
        writer.write_warnings(&doc.warnings)?;
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
    loader: &'a DocumentLoader<'a>,
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

    let source = args.loader.open_source(args.path)?;
    let doc = args.loader.load(&source)?;
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
