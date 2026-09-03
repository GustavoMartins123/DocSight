use clap::{Parser, Subcommand};
use docsight_agent::AgentEnvelope;
use docsight_core::{
    BlockContent, Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, ObjectId,
    Rect, table_to_csv, table_to_html, table_to_markdown, table_to_tsv, table_to_tsv_string,
};
use docsight_ooxml::parse_docx;
use docsight_pdf::{ENGINE_NAME, PdfDocument};
use docsight_render::{RenderRequest, RenderTarget, render_pdf};
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
    #[command(subcommand)]
    command: Command,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum TableFormat {
    Json,
    Markdown,
    Csv,
    Html,
    Tsv,
}

#[derive(Debug, Serialize)]
struct InspectCapabilities {
    structure: bool,
    text: bool,
    render: bool,
}

#[derive(Debug, Serialize)]
struct InspectResult {
    format: DocumentFormat,
    size_bytes: u64,
    capabilities: InspectCapabilities,
    paragraphs: Option<usize>,
    headings: Option<usize>,
    tables: Option<usize>,
    pages: Option<u32>,
    engine: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct HeadingRecord {
    id: ObjectId,
    level: u8,
    text: String,
    source: String,
}

#[derive(Debug, Serialize)]
struct OutlineResult {
    headings: Vec<HeadingRecord>,
}

#[derive(Debug, Serialize)]
struct TextRecord {
    id: String,
    kind: &'static str,
    text: String,
}

#[derive(Debug, Serialize)]
struct TextResult {
    blocks: Vec<TextRecord>,
}

#[derive(Debug, Serialize)]
struct TableSummary {
    id: String,
    rows: u32,
    columns: u32,
    source: String,
}

#[derive(Debug, Serialize)]
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
struct PageResult {
    number: u32,
    width_pt: f32,
    height_pt: f32,
    spans: Vec<PageSpanRecord>,
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
    match &cli.command {
        Command::Inspect { path, json } => inspect(path, *json),
        Command::Outline { path, json } => outline(path, *json),
        Command::Text { path, json } => document_text(path, *json),
        Command::Tables { path, json } => tables(path, *json),
        Command::Table {
            path,
            object,
            format,
        } => table(path, object, *format),
        Command::Page { path, page, json } => page_command(path, *page, *json),
        Command::Render {
            path,
            page,
            dpi,
            out,
        } => render(path, RenderTarget::Page { page: *page }, *dpi, out),
        Command::Crop {
            path,
            page,
            bbox,
            object,
            dpi,
            out,
        } => crop(path, *page, *bbox, object.as_deref(), *dpi, out),
    }
}

fn load_document(source: &DocumentSource) -> Result<Document, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => parse_docx(source),
        DocumentFormat::Pdf => {
            let pdf = PdfDocument::open(source)?;
            pdf.to_document()
        }
    }
}

fn inspect(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let is_pdf = source.format() == DocumentFormat::Pdf;
    let paragraphs = document.paragraphs().count() + document.list_items().count();
    let headings = document.headings().count();
    let tables = document.tables().count();
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
            render: is_pdf,
        },
        paragraphs: if is_pdf { None } else { Some(paragraphs) },
        headings: if is_pdf { None } else { Some(headings) },
        tables: if is_pdf { None } else { Some(tables) },
        pages,
        engine: if is_pdf { Some(ENGINE_NAME) } else { None },
    };
    if json {
        write_json(&source, result, document.warnings)
    } else {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        writeln!(
            writer,
            "DOCSIGHT {} | {}",
            env!("CARGO_PKG_VERSION"),
            source.format()
        )
        .map_err(stdout_error)?;
        writeln!(writer, "Digest  sha256:{}", source.sha256()).map_err(stdout_error)?;
        writeln!(writer, "Bytes   {}", source.size_bytes()).map_err(stdout_error)?;
        if let Some(paragraphs) = result.paragraphs {
            writeln!(writer, "Paragraphs  {paragraphs}").map_err(stdout_error)?;
        }
        if let Some(headings) = result.headings {
            writeln!(writer, "Headings    {headings}").map_err(stdout_error)?;
        }
        if let Some(tables) = result.tables {
            writeln!(writer, "Tables      {tables}").map_err(stdout_error)?;
        }
        if let Some(pages) = result.pages {
            writeln!(writer, "Pages       {pages}").map_err(stdout_error)?;
        }
        if let Some(engine) = result.engine {
            writeln!(writer, "PDF engine  {engine}").map_err(stdout_error)?;
        }
        emit_warnings(&document.warnings)
    }
}

fn page_command(path: &PathBuf, number: u32, json: bool) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    if document.pages.is_empty() {
        return Err(DocsightError::UnsupportedFeature {
            feature: "unpaginated document layout required for page inspection".to_owned(),
        });
    }
    let target_page = document
        .page(number)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("page {number}"),
        })?;
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
    let page_result = PageResult {
        number: target_page.number,
        width_pt: target_page.width_pt,
        height_pt: target_page.height_pt,
        spans,
    };
    if json {
        return write_json(&source, &page_result, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(
        writer,
        "Page {}  {}x{} pt  {} spans",
        page_result.number,
        page_result.width_pt,
        page_result.height_pt,
        page_result.spans.len()
    )
    .map_err(stdout_error)?;
    for span in &page_result.spans {
        writeln!(
            writer,
            "[{}] [{},{},{},{}] {}",
            span.id, span.bbox.x0, span.bbox.y0, span.bbox.x1, span.bbox.y1, span.text
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn render(path: &PathBuf, target: RenderTarget, dpi: u16, out: &Path) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let rendered = render_pdf(&source, &RenderRequest { target, dpi })?;
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
    emit_warnings(&rendered.warnings)
}

fn crop(
    path: &PathBuf,
    page: Option<u32>,
    bbox: Option<Rect>,
    object: Option<&str>,
    dpi: u16,
    out: &Path,
) -> Result<(), DocsightError> {
    let target = match (page, bbox, object) {
        (Some(page), Some(bbox), None) => RenderTarget::Region { page, bbox },
        (None, None, Some(id)) => RenderTarget::Object { id: id.to_owned() },
        _ => {
            return Err(DocsightError::InvalidArgument {
                message: "crop requires either --page with --bbox or only --object".to_owned(),
            });
        }
    };
    render(path, target, dpi, out)
}

fn outline(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
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
    if json {
        return write_json(&source, OutlineResult { headings }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for heading in &headings {
        let indentation = "  ".repeat(usize::from(heading.level.saturating_sub(1)));
        writeln!(writer, "{indentation}[{}] {}", heading.id, heading.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn document_text(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
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
                BlockContent::Unknown(u) => ("unknown", u.details.clone().unwrap_or_default()),
            };
            TextRecord {
                id: block.id.to_string(),
                kind,
                text,
            }
        })
        .collect();
    if json {
        return write_json(&source, TextResult { blocks }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for block in &blocks {
        writeln!(writer, "[{}] {} {}", block.id, block.kind, block.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn tables(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let tables: Vec<TableSummary> = document
        .tables()
        .map(|(block, table)| TableSummary {
            id: block.id.to_string(),
            rows: table.rows,
            columns: table.columns,
            source: block.source.path.clone(),
        })
        .collect();
    if json {
        return write_json(&source, TablesResult { tables }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for table in &tables {
        writeln!(
            writer,
            "{}  {}x{}  {}",
            table.id, table.rows, table.columns, table.source
        )
        .map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn table(path: &PathBuf, object: &str, format: TableFormat) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let document = load_document(&source)?;
    let (_block, selected) =
        document
            .find_table(object)
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: object.to_owned(),
            })?;
    match format {
        TableFormat::Json => write_json(&source, selected, document.warnings.clone()),
        TableFormat::Markdown => {
            let markdown = table_to_markdown(selected)?;
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            writer
                .write_all(markdown.as_bytes())
                .map_err(stdout_error)?;
            emit_warnings(&document.warnings)
        }
        TableFormat::Csv => {
            let csv = table_to_csv(selected)?;
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            writer.write_all(csv.as_bytes()).map_err(stdout_error)?;
            emit_warnings(&document.warnings)
        }
        TableFormat::Tsv => {
            let tsv = table_to_tsv(selected)?;
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            writer.write_all(tsv.as_bytes()).map_err(stdout_error)?;
            emit_warnings(&document.warnings)
        }
        TableFormat::Html => {
            let html = table_to_html(selected)?;
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            writer.write_all(html.as_bytes()).map_err(stdout_error)?;
            emit_warnings(&document.warnings)
        }
    }
}

fn write_json<T>(
    source: &DocumentSource,
    result: T,
    warnings: Vec<Diagnostic>,
) -> Result<(), DocsightError>
where
    T: Serialize,
{
    let envelope = AgentEnvelope::with_warnings(source, result, warnings);
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, &envelope).map_err(output_serialization_error)?;
    writer.write_all(b"\n").map_err(stdout_error)
}

fn emit_warnings(warnings: &[Diagnostic]) -> Result<(), DocsightError> {
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    for warning in warnings {
        writeln!(writer, "{}: {}", warning.code, warning.message).map_err(stderr_error)?;
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

fn parse_bbox(value: &str) -> Result<Rect, String> {
    let parts = value.split(',').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err("bbox must contain x0,y0,x1,y1".to_owned());
    }
    let coordinates = parts
        .iter()
        .map(|part| {
            part.parse::<f32>()
                .map_err(|_| "bbox coordinates must be finite numbers".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Rect::new(
        coordinates[0],
        coordinates[1],
        coordinates[2],
        coordinates[3],
    )
    .map_err(|error| error.to_string())
}
