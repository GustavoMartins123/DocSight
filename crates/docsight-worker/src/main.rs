use clap::{Parser, Subcommand};
use docsight_core::{Diagnostic, DocsightError, DocumentFormat, DocumentSource};
use docsight_ingest::ingest as load_doc;
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

    #[arg(long)]
    memory_hog_for_test: bool,

    #[arg(long, hide = true)]
    cpu_hog_for_test: bool,

    #[arg(long, hide = true)]
    network_probe_for_test: bool,

    #[arg(long, hide = true)]
    filesystem_probe_for_test: bool,

    #[arg(long, hide = true)]
    filesystem_write_probe_for_test: bool,

    #[arg(long, hide = true)]
    sleep_for_test: bool,

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
    if let Err(error) =
        docsight_worker::apply_sandbox_limits_if_child(&docsight_worker::SandboxPolicy::default())
    {
        let code = error.exit_code();
        return if writeln!(io::stderr(), "{}: {}", error.diagnostic().code, error).is_ok() {
            ExitCode::from(code)
        } else {
            ExitCode::from(40)
        };
    }
    let cli = WorkerCli::parse();
    if cli.crash_for_test {
        std::process::abort();
    }
    if cli.memory_hog_for_test {
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            buffer.resize(buffer.len().saturating_add(16 * 1024 * 1024), 0xAB);
        }
    }
    if cli.cpu_hog_for_test {
        let mut counter: u64 = 0;
        loop {
            counter = std::hint::black_box(counter.wrapping_add(1));
        }
    }
    if cli.network_probe_for_test {
        return network_probe_exit_code();
    }
    if cli.filesystem_probe_for_test {
        return filesystem_probe_exit_code();
    }
    if cli.filesystem_write_probe_for_test {
        return filesystem_write_probe_exit_code();
    }
    if cli.sleep_for_test {
        let milliseconds = std::env::var("DOCSIGHT_SLEEP_PROBE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60_000);
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
        return ExitCode::SUCCESS;
    }
    match execute_worker(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let code = err.exit_code();
            if writeln!(io::stderr(), "{}: {}", err.diagnostic().code, err).is_ok() {
                ExitCode::from(code)
            } else {
                ExitCode::from(40)
            }
        }
    }
}

const NETWORK_PROBE_ENDPOINT_ENV: &str = "DOCSIGHT_NETWORK_PROBE_ENDPOINT";

fn network_probe_exit_code() -> ExitCode {
    let Some(endpoint) = std::env::var_os(NETWORK_PROBE_ENDPOINT_ENV) else {
        return ExitCode::from(30);
    };
    let Some(endpoint) = endpoint.to_str() else {
        return ExitCode::from(30);
    };
    let Ok(address) = endpoint.parse::<std::net::SocketAddr>() else {
        return ExitCode::from(30);
    };
    match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(2)) {
        Err(error) if network_access_denied(&error) => ExitCode::SUCCESS,
        _ => ExitCode::from(30),
    }
}

#[cfg(unix)]
fn network_access_denied(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "windows")]
fn network_access_denied(error: &io::Error) -> bool {
    const WSAEACCES: i32 = 10013;
    error.kind() == io::ErrorKind::TimedOut || error.raw_os_error() == Some(WSAEACCES)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn network_access_denied(_error: &io::Error) -> bool {
    false
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn filesystem_probe_exit_code() -> ExitCode {
    let Some(path) = std::env::var_os("DOCSIGHT_FILESYSTEM_PROBE_PATH") else {
        return ExitCode::from(30);
    };
    match std::fs::read(path) {
        Err(error) if matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM)) => {
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(30),
    }
}

#[cfg(target_os = "windows")]
fn filesystem_probe_exit_code() -> ExitCode {
    const ERROR_ACCESS_DENIED: i32 = 5;
    let Some(path) = std::env::var_os("DOCSIGHT_FILESYSTEM_PROBE_PATH") else {
        return ExitCode::from(30);
    };
    match std::fs::read(path) {
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => ExitCode::SUCCESS,
        _ => ExitCode::from(30),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn filesystem_probe_exit_code() -> ExitCode {
    ExitCode::from(30)
}

const WRITE_PROBE_DIRECTORY_ENV: &str = "DOCSIGHT_FILESYSTEM_WRITE_PROBE_DIR";
const WRITE_PROBE_INPUT_ENV: &str = "DOCSIGHT_FILESYSTEM_WRITE_PROBE_INPUT";
const WRITE_PROBE_DENIED_ENV: &str = "DOCSIGHT_FILESYSTEM_WRITE_PROBE_DENIED";
const WRITE_PROBE_CONTENT: &[u8] = b"declared write path";

/// Mirrors how the cache hands an entry to an isolated worker: read a declared file, create and
/// write a new file in the declared directory, rename it there, and stay locked out elsewhere.
fn filesystem_write_probe_exit_code() -> ExitCode {
    let (Some(directory), Some(input), Some(denied)) = (
        std::env::var_os(WRITE_PROBE_DIRECTORY_ENV).map(PathBuf::from),
        std::env::var_os(WRITE_PROBE_INPUT_ENV).map(PathBuf::from),
        std::env::var_os(WRITE_PROBE_DENIED_ENV).map(PathBuf::from),
    ) else {
        return ExitCode::from(30);
    };
    if std::fs::read(&input).is_err() {
        return ExitCode::from(31);
    }
    let partial = directory.join("probe.partial");
    let write = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(&partial)
        .and_then(|mut file| file.write_all(WRITE_PROBE_CONTENT));
    if write.is_err() {
        return ExitCode::from(32);
    }
    if std::fs::rename(&partial, directory.join("probe.result")).is_err() {
        return ExitCode::from(33);
    }
    match std::fs::write(denied.join("probe.denied"), WRITE_PROBE_CONTENT) {
        Err(error) if write_access_denied(&error) => ExitCode::SUCCESS,
        _ => ExitCode::from(34),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_access_denied(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM))
}

#[cfg(target_os = "windows")]
fn write_access_denied(error: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    error.raw_os_error() == Some(ERROR_ACCESS_DENIED)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn write_access_denied(_error: &io::Error) -> bool {
    false
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

fn stdout_err(source: io::Error) -> DocsightError {
    DocsightError::Io {
        path: PathBuf::from("<stdout>"),
        source,
    }
}
