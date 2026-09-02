use clap::{Parser, Subcommand};
use docsight_agent::AgentEnvelope;
use docsight_core::{Diagnostic, DocsightError, DocumentFormat, DocumentSource};
use docsight_ooxml::{DocxBlock, DocxDocument, Heading, Table, parse_docx};
use serde::Serialize;
use std::io::{self, Write};
use std::path::PathBuf;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum TableFormat {
    Json,
    Markdown,
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
}

#[derive(Debug, Serialize)]
struct OutlineResult {
    headings: Vec<Heading>,
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
    }
}

fn inspect(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let parsed = if source.format() == DocumentFormat::Docx {
        Some(parse_docx(&source)?)
    } else {
        None
    };
    let result = InspectResult {
        format: source.format(),
        size_bytes: source.size_bytes(),
        capabilities: InspectCapabilities {
            structure: source.format() == DocumentFormat::Docx,
            text: source.format() == DocumentFormat::Docx,
            render: false,
        },
        paragraphs: parsed
            .as_ref()
            .map(|document| document.paragraphs().count()),
        headings: parsed.as_ref().map(|document| document.headings().count()),
        tables: parsed.as_ref().map(|document| document.tables().count()),
    };
    let warnings = parsed
        .as_ref()
        .map_or_else(Vec::new, |document| document.warnings.clone());
    if json {
        write_json(&source, result, warnings)
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
        if let Some(document) = parsed {
            writeln!(writer, "Paragraphs  {}", document.paragraphs().count())
                .map_err(stdout_error)?;
            writeln!(writer, "Headings    {}", document.headings().count())
                .map_err(stdout_error)?;
            writeln!(writer, "Tables      {}", document.tables().count()).map_err(stdout_error)?;
            emit_warnings(&document.warnings)?;
        }
        Ok(())
    }
}

fn outline(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let (source, document) = load_docx(path, "outline")?;
    let headings: Vec<_> = document.headings().collect();
    if json {
        return write_json(&source, OutlineResult { headings }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for heading in headings {
        let indentation = "  ".repeat(usize::from(heading.level.saturating_sub(1)));
        writeln!(writer, "{indentation}[{}] {}", heading.id, heading.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn document_text(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let (source, document) = load_docx(path, "text")?;
    let blocks = text_records(&document);
    if json {
        return write_json(&source, TextResult { blocks }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for block in blocks {
        writeln!(writer, "[{}] {} {}", block.id, block.kind, block.text).map_err(stdout_error)?;
    }
    emit_warnings(&document.warnings)
}

fn tables(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let (source, document) = load_docx(path, "tables")?;
    let tables: Vec<_> = document
        .tables()
        .map(|table| TableSummary {
            id: table.id.to_string(),
            rows: table.rows,
            columns: table.columns,
            source: table.source.clone(),
        })
        .collect();
    if json {
        return write_json(&source, TablesResult { tables }, document.warnings);
    }
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for table in tables {
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
    let (source, document) = load_docx(path, "table")?;
    let selected = document
        .tables()
        .find(|table| table.id.as_str() == object)
        .cloned()
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: object.to_owned(),
        })?;
    match format {
        TableFormat::Json => write_json(&source, selected, document.warnings),
        TableFormat::Markdown => {
            let markdown = table_to_markdown(&selected)?;
            let stdout = io::stdout();
            let mut writer = stdout.lock();
            writer
                .write_all(markdown.as_bytes())
                .map_err(stdout_error)?;
            emit_warnings(&document.warnings)
        }
    }
}

fn load_docx(
    path: &PathBuf,
    operation: &str,
) -> Result<(DocumentSource, DocxDocument), DocsightError> {
    let source = DocumentSource::open(path)?;
    if source.format() != DocumentFormat::Docx {
        return Err(DocsightError::UnsupportedOperation {
            operation: operation.to_owned(),
            format: source.format(),
        });
    }
    let document = parse_docx(&source)?;
    Ok((source, document))
}

fn text_records(document: &DocxDocument) -> Vec<TextRecord> {
    document
        .blocks
        .iter()
        .map(|block| match block {
            DocxBlock::Paragraph(paragraph) => TextRecord {
                id: paragraph.id.to_string(),
                kind: match paragraph.kind {
                    docsight_ooxml::ParagraphKind::Heading => "heading",
                    docsight_ooxml::ParagraphKind::ListItem => "list_item",
                    docsight_ooxml::ParagraphKind::Paragraph => "paragraph",
                },
                text: paragraph.text.clone(),
            },
            DocxBlock::Table(table) => TextRecord {
                id: table.id.to_string(),
                kind: "table",
                text: table_text(table),
            },
        })
        .collect()
}

fn table_text(table: &Table) -> String {
    let mut rows = vec![Vec::new(); table.rows as usize];
    for cell in &table.cells {
        if let Some(row) = rows.get_mut(cell.row as usize) {
            row.push(cell.text.clone());
        }
    }
    rows.into_iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_to_markdown(table: &Table) -> Result<String, DocsightError> {
    let columns = usize::try_from(table.columns).map_err(|_| DocsightError::ResourceLimit {
        resource: "table columns".to_owned(),
        limit: usize::MAX as u64,
    })?;
    let rows = usize::try_from(table.rows).map_err(|_| DocsightError::ResourceLimit {
        resource: "table rows".to_owned(),
        limit: usize::MAX as u64,
    })?;
    if columns == 0 {
        return Ok(String::new());
    }
    let mut grid = vec![vec![String::new(); columns]; rows];
    for cell in &table.cells {
        let row = usize::try_from(cell.row).map_err(|_| table_geometry_error())?;
        let column = usize::try_from(cell.column).map_err(|_| table_geometry_error())?;
        let target = grid
            .get_mut(row)
            .and_then(|values| values.get_mut(column))
            .ok_or_else(table_geometry_error)?;
        let mut value = cell.text.replace('|', "\\|").replace('\n', "<br>");
        if cell.row_span > 1 || cell.column_span > 1 {
            value.push_str(&format!(" [span {}x{}]", cell.row_span, cell.column_span));
        }
        *target = value;
    }
    let mut markdown = String::new();
    markdown.push('|');
    for column in 1..=columns {
        markdown.push_str(&format!(" Column {column} |"));
    }
    markdown.push('\n');
    markdown.push('|');
    for _ in 0..columns {
        markdown.push_str(" --- |");
    }
    markdown.push('\n');
    for row in grid {
        markdown.push('|');
        for value in row {
            markdown.push(' ');
            markdown.push_str(&value);
            markdown.push_str(" |");
        }
        markdown.push('\n');
    }
    Ok(markdown)
}

fn table_geometry_error() -> DocsightError {
    DocsightError::MalformedDocument {
        message: "table cell coordinates exceed the declared table grid".to_owned(),
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
