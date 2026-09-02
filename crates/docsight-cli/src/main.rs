use clap::{Parser, Subcommand};
use docsight_agent::AgentEnvelope;
use docsight_core::{DocsightError, DocumentFormat, DocumentSource};
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
    }
}

fn inspect(path: &PathBuf, json: bool) -> Result<(), DocsightError> {
    let source = DocumentSource::open(path)?;
    let result = InspectResult {
        format: source.format(),
        size_bytes: source.size_bytes(),
        capabilities: InspectCapabilities {
            structure: source.format() == DocumentFormat::Docx,
            text: source.format() == DocumentFormat::Docx,
            render: source.format() == DocumentFormat::Pdf,
        },
    };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if json {
        let envelope = AgentEnvelope::complete(&source, result);
        serde_json::to_writer(&mut writer, &envelope).map_err(output_serialization_error)?;
        writer.write_all(b"\n").map_err(stdout_error)?;
    } else {
        writeln!(
            writer,
            "DOCSIGHT {} | {}",
            env!("CARGO_PKG_VERSION"),
            source.format()
        )
        .map_err(stdout_error)?;
        writeln!(writer, "Digest  sha256:{}", source.sha256()).map_err(stdout_error)?;
        writeln!(writer, "Bytes   {}", source.size_bytes()).map_err(stdout_error)?;
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
