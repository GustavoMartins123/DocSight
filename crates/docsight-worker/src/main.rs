use clap::{Parser, Subcommand};
use docsight_core::{Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_render::{RenderRequest, RenderTarget, render_document};
use serde::Serialize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "docsight-worker",
    about = "Isolated worker backend for DOCSIGHT"
)]
struct WorkerCli {
    #[arg(long)]
    json: bool,

    #[arg(long)]
    crash_for_test: bool,

    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    Inspect {
        path: PathBuf,
    },
    Text {
        path: PathBuf,
    },
    Outline {
        path: PathBuf,
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
}

#[derive(Serialize)]
struct WorkerInspectResult {
    format: &'static str,
    sha256: String,
    pages: u32,
    paragraphs: usize,
    headings: usize,
    tables: usize,
    warnings: Vec<Diagnostic>,
}

fn main() -> ExitCode {
    let cli = WorkerCli::parse();
    if cli.crash_for_test {
        std::process::abort();
    }
    match execute_worker(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let code = err.exit_code();
            let _ = writeln!(io::stderr(), "{}: {}", err.diagnostic().code, err);
            ExitCode::from(code)
        }
    }
}

fn execute_worker(cli: &WorkerCli) -> Result<(), DocsightError> {
    match &cli.command {
        WorkerCommand::Inspect { path } => {
            let source = DocumentSource::open(path)?;
            let doc = load_doc(&source)?;
            let format_str = match source.format() {
                DocumentFormat::Docx => "docx",
                DocumentFormat::Pdf => "pdf",
            };
            let result = WorkerInspectResult {
                format: format_str,
                sha256: source.sha256().to_owned(),
                pages: doc.pages.len() as u32,
                paragraphs: doc.paragraphs().count(),
                headings: doc.headings().count(),
                tables: doc.tables().count(),
                warnings: doc.warnings,
            };
            if cli.json {
                let json = serde_json::to_string_pretty(&result).map_err(|e| {
                    DocsightError::MalformedDocument {
                        message: format!("serialization error: {e}"),
                    }
                })?;
                writeln!(io::stdout(), "{json}").map_err(|e| DocsightError::Io {
                    path: PathBuf::from("<stdout>"),
                    source: e,
                })?;
            } else {
                writeln!(io::stdout(), "Format:     {}", result.format).map_err(stdout_err)?;
                writeln!(io::stdout(), "Pages:      {}", result.pages).map_err(stdout_err)?;
                writeln!(io::stdout(), "Paragraphs: {}", result.paragraphs).map_err(stdout_err)?;
                writeln!(io::stdout(), "Headings:   {}", result.headings).map_err(stdout_err)?;
                writeln!(io::stdout(), "Tables:     {}", result.tables).map_err(stdout_err)?;
            }
            Ok(())
        }
        WorkerCommand::Text { path } => {
            let source = DocumentSource::open(path)?;
            let doc = load_doc(&source)?;
            let mut out = io::stdout().lock();
            for block in &doc.blocks {
                let t = block.text();
                if !t.trim().is_empty() {
                    let kind_str = format!("{:?}", block.kind).to_lowercase();
                    writeln!(out, "[{}] {} {}", block.id, kind_str, t).map_err(stdout_err)?;
                }
            }
            Ok(())
        }
        WorkerCommand::Outline { path } => {
            let source = DocumentSource::open(path)?;
            let doc = load_doc(&source)?;
            let mut out = io::stdout().lock();
            for (block, h) in doc.headings() {
                let indent = "  ".repeat(h.level.saturating_sub(1) as usize);
                writeln!(out, "{indent}[{}] {}", block.id, h.text).map_err(stdout_err)?;
            }
            Ok(())
        }
        WorkerCommand::Render {
            path,
            page,
            dpi,
            out,
        } => {
            let source = DocumentSource::open(path)?;
            let req = RenderRequest {
                target: RenderTarget::Page { page: *page },
                dpi: *dpi,
            };
            let img = render_document(&source, &req)?;
            img.write(out)?;
            writeln!(io::stdout(), "Rendered page {} to {}", page, out.display())
                .map_err(stdout_err)?;
            Ok(())
        }
    }
}

fn load_doc(source: &DocumentSource) -> Result<Document, DocsightError> {
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

fn stdout_err(source: io::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stdout>"),
        source,
    }
}
