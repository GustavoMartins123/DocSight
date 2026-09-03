use clap::{Parser, Subcommand};
use docsight_agent::{
    AgentEnvelope, NdjsonWriter, OutputLimits, QueryLimits, apply_bounded_collection, project_json,
    truncate_json_strings,
};
use docsight_core::{
    BlockContent, Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, ObjectId,
    Rect, table_to_csv, table_to_html, table_to_markdown, table_to_tsv, table_to_tsv_string,
};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::{ENGINE_NAME, PdfDocument};
use docsight_render::{RenderRequest, RenderTarget, render_document};
use serde::Serialize;
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
    #[arg(long, global = true)]
    json_errors: bool,

    #[arg(long, global = true)]
    ndjson: bool,

    #[arg(long, global = true)]
    max_bytes: Option<usize>,

    #[arg(long, global = true)]
    max_items: Option<usize>,

    #[arg(long, global = true)]
    text_limit: Option<usize>,

    #[arg(long = "continue", global = true)]
    continue_token: Option<String>,

    #[arg(long, global = true, value_delimiter = ',')]
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
        subcommand_json
            || self.max_bytes.is_some()
            || self.max_items.is_some()
            || self.text_limit.is_some()
            || self.continue_token.is_some()
            || self.select.is_some()
    }
}

#[derive(Debug, Subcommand)]
enum Command {
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
struct InspectResult {
    format: DocumentFormat,
    size_bytes: u64,
    capabilities: InspectCapabilities,
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let exit_code = error.exit_code();
            if emit_error(&error, cli.json_errors).is_err() {
                return ExitCode::from(40);
            }
            ExitCode::from(exit_code)
        }
    }
}

fn execute(cli: &Cli) -> Result<(), DocsightError> {
    let limits = cli.query_limits();
    match &cli.command {
        Command::Inspect { path, json } => inspect(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Outline { path, json } => outline(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Text { path, json } => document_text(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Tables { path, json } => tables(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Table {
            path,
            object,
            format,
        } => table(path, object, *format, cli.quiet, cli.json_errors),
        Command::Page { path, page, json } => page_command(
            path,
            *page,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Render {
            path,
            page,
            dpi,
            out,
        } => render(
            path,
            RenderTarget::Page { page: *page },
            *dpi,
            out,
            cli.quiet,
            cli.json_errors,
        ),
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
            render(path, target, *dpi, out, cli.quiet, cli.json_errors)
        }
        Command::Images { path, json } => images(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
        Command::Links { path, json } => links(
            path,
            cli.is_agent_json(*json),
            cli.ndjson,
            &limits,
            cli.quiet,
            cli.json_errors,
        ),
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

fn inspect(
    path: &PathBuf,
    json: bool,
    ndjson: bool,
    limits: &QueryLimits,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let is_pdf = source.format() == DocumentFormat::Pdf;
    let paragraphs = document.paragraphs().count() + document.list_items().count();
    let headings = document.headings().count();
    let tables = document.tables().count();
    let figures = document.figures().count();
    let comments = document.comments.len();
    let tracked =
        if document.tracked_changes.insertions > 0 || document.tracked_changes.deletions > 0 {
            Some(document.tracked_changes)
        } else {
            None
        };
    let pages = if document.pages.is_empty() {
        None
    } else {
        Some(document.pages.len() as u32)
    };
    let result = InspectResult {
        format: source.format(),
        size_bytes: source.size_bytes(),
        capabilities: InspectCapabilities {
            structure: true,
            text: true,
            render: true,
        },
        paragraphs: Some(paragraphs),
        headings: Some(headings),
        tables: Some(tables),
        figures: Some(figures),
        comments: if is_pdf { None } else { Some(comments) },
        tracked,
        pages,
        engine: if is_pdf { Some(ENGINE_NAME) } else { None },
    };

    if ndjson {
        let stdout = io::stdout();
        let mut writer = NdjsonWriter::new(
            stdout.lock(),
            limits.clone(),
            "inspect".into(),
            source.sha256().to_owned(),
        )?;
        writer.write_meta(&(&source).into())?;
        let val = serde_json::to_value(&result).map_err(output_serialization_error)?;
        writer.write_item("inspect", &val)?;
        for warning in &document.warnings {
            writer.write_warning(warning)?;
        }
        writer.finish(1)?;
        return Ok(());
    }

    if json {
        return write_single_json(&source, &result, document.warnings, limits);
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
    emit_warnings(&document.warnings, quiet, json_errors)
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
        writer.finish(headings.len())?;
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
        )?;
        writer.write_meta(&(&source).into())?;
        writer.write_page_begin(number)?;
        let offset = writer.continuation_offset();
        let total_items = spans.len() + overlays.len();
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
        writer.finish(total_items)?;
        return Ok(());
    }

    if json {
        let envelope =
            apply_bounded_collection(&spans, limits, "page", &source, document.warnings, |s| {
                serde_json::to_value(PageResult {
                    number: target_page_number,
                    width_pt: target_page_width,
                    height_pt: target_page_height,
                    spans: s,
                    overlays: overlays.clone(),
                })
                .map_err(output_serialization_error)
            })?;
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
        writer.finish(blocks.len())?;
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
    let is_pdf = source.format() == DocumentFormat::Pdf;
    let tables: Vec<TableSummary> = document
        .tables()
        .map(|(block, table)| {
            let detector = if is_pdf {
                if block.source.path.ends_with("ruled") {
                    Some("ruled".to_owned())
                } else if block.source.path.ends_with("alignment") {
                    Some("alignment".to_owned())
                } else {
                    Some("inferred".to_owned())
                }
            } else {
                Some("structural".to_owned())
            };
            TableSummary {
                id: block.id.to_string(),
                page: block.page,
                rows: table.rows,
                columns: table.columns,
                confidence: Some(block.confidence),
                detector,
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
        writer.finish(tables.len())?;
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
            let review_suffix =
                if table.confidence.map(|c| c < 0.70).unwrap_or(false) && det_str != "structural" {
                    "  [review]"
                } else {
                    ""
                };
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

fn table(
    path: &PathBuf,
    object: &str,
    format: TableFormat,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let target = document
        .tables()
        .find(|(block, _)| block.id.to_string() == object)
        .map(|(_, table)| table)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: object.to_owned(),
        })?;

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match format {
        TableFormat::Json => {
            return write_single_json(
                &source,
                target,
                document.warnings.clone(),
                &QueryLimits::default(),
            );
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
    emit_warnings(&document.warnings, quiet, json_errors)
}

fn render(
    path: &PathBuf,
    target: RenderTarget,
    dpi: u16,
    out: &Path,
    quiet: bool,
    json_errors: bool,
) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let rendered = render_document(&source, &RenderRequest { target, dpi })?;
    rendered.write(out)?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Rendered page {} at {} DPI to {} ({}x{} px)",
        rendered.metadata.page,
        rendered.metadata.dpi,
        out.display(),
        rendered.metadata.width_px,
        rendered.metadata.height_px
    )
    .map_err(stdout_error)?;
    emit_warnings(&rendered.warnings, quiet, json_errors)
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
        writer.finish(images.len())?;
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
        writer.finish(links.len())?;
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
    if let Some(text_limit) = limits.text_limit {
        truncate_json_strings(&mut val, text_limit);
    }
    if let Some(ref select) = limits.select {
        val = project_json(&val, select);
    }
    let envelope = AgentEnvelope::with_limits(source, val, warnings, OutputLimits::default());
    write_envelope(&envelope)
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

fn emit_error(error: &DocsightError, json: bool) -> io::Result<()> {
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    let diagnostic = error.diagnostic();
    if json {
        serde_json::to_writer(&mut writer, &diagnostic).map_err(io::Error::other)?;
        writer.write_all(b"\n")
    } else {
        writeln!(writer, "{}: {}", diagnostic.code, diagnostic.message)
    }
}

fn output_serialization_error(source: serde_json::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stdout>"),
        source: io::Error::other(source),
    }
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
