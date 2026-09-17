use docsight_core::{Document, DocumentFormat, DocumentSource};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_render::{RenderRequest, RenderTarget, render_document};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as FmtWrite;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

type TaskResult<T> = Result<T, Box<dyn Error>>;

const REPORT_SCHEMA: &str = "docsight.performance-report/v1";
const BUDGET_SCHEMA: &str = "docsight.performance-budgets/v1";
const DEFAULT_ITERATIONS: u32 = 3;

#[derive(Clone, Copy)]
enum FixtureKind {
    SampleDocx,
    SpecificationDocx,
    GeneratedDocx { blocks: usize },
    GeneratedPdf { pages: usize },
    LargeImagePdf { width: u32, height: u32 },
}

#[derive(Clone, Copy)]
struct ScenarioDefinition {
    name: &'static str,
    document_class: &'static str,
    workload: &'static str,
    fixture: FixtureKind,
}

const SCENARIOS: &[ScenarioDefinition] = &[
    ScenarioDefinition {
        name: "docx_small",
        document_class: "small",
        workload: "parse_layout_render",
        fixture: FixtureKind::SampleDocx,
    },
    ScenarioDefinition {
        name: "docx_medium",
        document_class: "medium",
        workload: "parse_layout_render",
        fixture: FixtureKind::SpecificationDocx,
    },
    ScenarioDefinition {
        name: "docx_5000_blocks",
        document_class: "large",
        workload: "parse_layout_render",
        fixture: FixtureKind::GeneratedDocx { blocks: 5_000 },
    },
    ScenarioDefinition {
        name: "pdf_1_page",
        document_class: "small",
        workload: "parse_normalize_render",
        fixture: FixtureKind::GeneratedPdf { pages: 1 },
    },
    ScenarioDefinition {
        name: "pdf_100_pages",
        document_class: "medium",
        workload: "parse_normalize_render",
        fixture: FixtureKind::GeneratedPdf { pages: 100 },
    },
    ScenarioDefinition {
        name: "pdf_1000_pages",
        document_class: "large",
        workload: "parse_normalize_render",
        fixture: FixtureKind::GeneratedPdf { pages: 1_000 },
    },
    ScenarioDefinition {
        name: "pdf_large_image",
        document_class: "large_image",
        workload: "parse_normalize_render",
        fixture: FixtureKind::LargeImagePdf {
            width: 2_048,
            height: 2_048,
        },
    },
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PerformanceBudgets {
    schema: String,
    reference: ReferenceEnvironment,
    iterations: u32,
    scenarios: Vec<ScenarioBudget>,
    growth_limits: Vec<GrowthBudget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceEnvironment {
    os: String,
    architecture: String,
    profile: String,
    rust: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioBudget {
    name: String,
    minimum_pages: u64,
    minimum_objects: u64,
    maximum_parse_time_ms: u64,
    maximum_layout_or_normalize_time_ms: u64,
    maximum_render_time_ms: u64,
    maximum_wall_time_ms: u64,
    maximum_peak_memory_bytes: u64,
    maximum_output_size_bytes: u64,
    minimum_objects_per_second: f64,
    minimum_pages_per_second: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrowthBudget {
    baseline: String,
    scaled: String,
    maximum_peak_memory_ratio: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerMetrics {
    input_size_bytes: u64,
    parse_time_ns: u64,
    layout_time_ns: Option<u64>,
    normalize_time_ns: Option<u64>,
    render_time_ns: u64,
    output_size_bytes: u64,
    ir_size_bytes: u64,
    png_size_bytes: u64,
    pages: u64,
    objects: u64,
}

#[derive(Clone, Debug, Serialize)]
struct ScenarioReport {
    name: String,
    document_class: String,
    workload: String,
    iterations: u32,
    input_size_bytes: u64,
    parse_time_ms: f64,
    layout_time_ms: Option<f64>,
    normalize_time_ms: Option<f64>,
    render_time_ms: f64,
    wall_time_ms: f64,
    peak_memory_bytes: u64,
    output_size_bytes: u64,
    ir_size_bytes: u64,
    png_size_bytes: u64,
    pages: u64,
    objects: u64,
    objects_per_second: f64,
    pages_per_second: f64,
}

#[derive(Debug, Serialize)]
struct HostEnvironment {
    os: &'static str,
    architecture: &'static str,
    profile: &'static str,
}

#[derive(Debug, Serialize)]
struct PerformanceReport {
    schema: &'static str,
    engine: &'static str,
    host: HostEnvironment,
    scenarios: Vec<ScenarioReport>,
}

struct WorkerSample {
    metrics: WorkerMetrics,
    wall_time_ns: u64,
    peak_memory_bytes: u64,
}

struct PreparedFixtures {
    _directory: TempDir,
    paths: BTreeMap<&'static str, PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> TaskResult<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("benchmark") => run_benchmark_command(arguments.collect()),
        Some("__benchmark-worker") => {
            let path = arguments
                .next()
                .ok_or_else(|| failure("benchmark worker requires a document path"))?;
            if arguments.next().is_some() {
                return Err(failure("benchmark worker received unexpected arguments"));
            }
            let metrics = measure_document(Path::new(&path))?;
            serde_json::to_writer(io::stdout().lock(), &metrics)?;
            Ok(())
        }
        Some(command) => Err(failure(format!("unknown xtask command: {command}"))),
        None => Err(failure(
            "usage: cargo run --locked --release -p xtask --bin xtask -- benchmark [--check] [--budgets PATH] [--output PATH]",
        )),
    }
}

fn run_benchmark_command(arguments: Vec<String>) -> TaskResult<()> {
    let mut check = false;
    let mut budgets_path = PathBuf::from("benchmarks/ds9-budgets.json");
    let mut output_path = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--check" => check = true,
            "--budgets" => {
                index += 1;
                budgets_path = PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| failure("--budgets requires a path"))?,
                );
            }
            "--output" => {
                index += 1;
                output_path = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| failure("--output requires a path"))?,
                ));
            }
            argument => return Err(failure(format!("unknown benchmark argument: {argument}"))),
        }
        index += 1;
    }
    ensure_supported_host()?;
    let budgets = read_budgets(&budgets_path)?;
    validate_budgets(&budgets)?;
    let iterations = if check {
        budgets.iterations
    } else {
        DEFAULT_ITERATIONS
    };
    let fixtures = prepare_fixtures()?;
    let executable = std::env::current_exe()?;
    let mut reports = Vec::with_capacity(SCENARIOS.len());
    for definition in SCENARIOS {
        let path = fixtures
            .paths
            .get(definition.name)
            .ok_or_else(|| failure(format!("fixture was not prepared: {}", definition.name)))?;
        reports.push(measure_scenario(&executable, definition, path, iterations)?);
    }
    let report = PerformanceReport {
        schema: REPORT_SCHEMA,
        engine: env!("CARGO_PKG_VERSION"),
        host: HostEnvironment {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            profile: "release",
        },
        scenarios: reports,
    };
    let encoded = serde_json::to_vec_pretty(&report)?;
    if let Some(path) = output_path {
        std::fs::write(path, &encoded)?;
    }
    io::stdout().lock().write_all(&encoded)?;
    io::stdout().lock().write_all(b"\n")?;
    if check {
        let violations = budget_violations(&report, &budgets)?;
        if !violations.is_empty() {
            return Err(failure(format!(
                "performance budget violations:\n{}",
                violations.join("\n")
            )));
        }
    }
    Ok(())
}

fn ensure_supported_host() -> TaskResult<()> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(())
    } else {
        Err(failure(
            "the canonical performance benchmark requires Linux x86_64 for comparable peak-memory readings",
        ))
    }
}

fn read_budgets(path: &Path) -> TaskResult<PerformanceBudgets> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_budgets(budgets: &PerformanceBudgets) -> TaskResult<()> {
    if budgets.schema != BUDGET_SCHEMA {
        return Err(failure(format!(
            "unexpected performance budget schema: {}",
            budgets.schema
        )));
    }
    if budgets.reference.os != "linux"
        || budgets.reference.architecture != "x86_64"
        || budgets.reference.profile != "release"
        || budgets.reference.rust != "1.96.0"
    {
        return Err(failure(
            "performance budgets must target Linux x86_64, release, Rust 1.96.0",
        ));
    }
    if budgets.iterations == 0 {
        return Err(failure("performance budget iterations must be positive"));
    }
    let expected = SCENARIOS
        .iter()
        .map(|scenario| scenario.name)
        .collect::<BTreeSet<_>>();
    let actual = budgets
        .scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect::<BTreeSet<_>>();
    if expected != actual || actual.len() != budgets.scenarios.len() {
        return Err(failure(
            "performance budgets must contain every scenario exactly once",
        ));
    }
    for growth in &budgets.growth_limits {
        if !actual.contains(growth.baseline.as_str()) || !actual.contains(growth.scaled.as_str()) {
            return Err(failure(format!(
                "growth limit references an unknown scenario: {} -> {}",
                growth.baseline, growth.scaled
            )));
        }
        if !growth.maximum_peak_memory_ratio.is_finite() || growth.maximum_peak_memory_ratio <= 0.0
        {
            return Err(failure("peak-memory growth ratios must be positive"));
        }
    }
    Ok(())
}

fn prepare_fixtures() -> TaskResult<PreparedFixtures> {
    let directory = tempfile::tempdir()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| failure("xtask has no workspace parent"))?
        .to_path_buf();
    let mut paths = BTreeMap::new();
    for scenario in SCENARIOS {
        let path = match scenario.fixture {
            FixtureKind::SampleDocx => root.join("fixtures/validation/sample_headings.docx"),
            FixtureKind::SpecificationDocx => root.join("Projeto_DOCSIGHT_Especificacao.docx"),
            FixtureKind::GeneratedDocx { blocks } => {
                let path = directory.path().join(format!("{blocks}-blocks.docx"));
                std::fs::write(&path, build_docx(blocks)?)?;
                path
            }
            FixtureKind::GeneratedPdf { pages } => {
                let path = directory.path().join(format!("{pages}-pages.pdf"));
                std::fs::write(&path, build_multipage_pdf(pages)?)?;
                path
            }
            FixtureKind::LargeImagePdf { width, height } => {
                let path = directory.path().join(format!("{width}x{height}-image.pdf"));
                std::fs::write(&path, build_large_image_pdf(width, height)?)?;
                path
            }
        };
        if !path.is_file() {
            return Err(failure(format!(
                "performance fixture does not exist: {}",
                path.display()
            )));
        }
        paths.insert(scenario.name, path);
    }
    Ok(PreparedFixtures {
        _directory: directory,
        paths,
    })
}

fn measure_scenario(
    executable: &Path,
    definition: &ScenarioDefinition,
    path: &Path,
    iterations: u32,
) -> TaskResult<ScenarioReport> {
    let mut samples = Vec::with_capacity(usize::try_from(iterations)?);
    for _ in 0..iterations {
        samples.push(run_worker(executable, path)?);
    }
    let first = samples
        .first()
        .ok_or_else(|| failure("benchmark scenario produced no samples"))?;
    for sample in &samples[1..] {
        if sample.metrics.input_size_bytes != first.metrics.input_size_bytes
            || sample.metrics.output_size_bytes != first.metrics.output_size_bytes
            || sample.metrics.ir_size_bytes != first.metrics.ir_size_bytes
            || sample.metrics.png_size_bytes != first.metrics.png_size_bytes
            || sample.metrics.pages != first.metrics.pages
            || sample.metrics.objects != first.metrics.objects
            || sample.metrics.layout_time_ns.is_some() != first.metrics.layout_time_ns.is_some()
            || sample.metrics.normalize_time_ns.is_some()
                != first.metrics.normalize_time_ns.is_some()
        {
            return Err(failure(format!(
                "benchmark scenario produced inconsistent results: {}",
                definition.name
            )));
        }
    }
    let parse_time_ns = median(samples.iter().map(|sample| sample.metrics.parse_time_ns))?;
    let layout_time_ns =
        median_optional(samples.iter().map(|sample| sample.metrics.layout_time_ns))?;
    let normalize_time_ns = median_optional(
        samples
            .iter()
            .map(|sample| sample.metrics.normalize_time_ns),
    )?;
    let render_time_ns = median(samples.iter().map(|sample| sample.metrics.render_time_ns))?;
    let processing_time_ns = parse_time_ns
        .saturating_add(layout_time_ns.unwrap_or_default())
        .saturating_add(normalize_time_ns.unwrap_or_default());
    let wall_time_ns = median(samples.iter().map(|sample| sample.wall_time_ns))?;
    let peak_memory_bytes = samples
        .iter()
        .map(|sample| sample.peak_memory_bytes)
        .max()
        .ok_or_else(|| failure("benchmark scenario has no memory sample"))?;
    Ok(ScenarioReport {
        name: definition.name.to_owned(),
        document_class: definition.document_class.to_owned(),
        workload: definition.workload.to_owned(),
        iterations,
        input_size_bytes: first.metrics.input_size_bytes,
        parse_time_ms: milliseconds(parse_time_ns),
        layout_time_ms: layout_time_ns.map(milliseconds),
        normalize_time_ms: normalize_time_ns.map(milliseconds),
        render_time_ms: milliseconds(render_time_ns),
        wall_time_ms: milliseconds(wall_time_ns),
        peak_memory_bytes,
        output_size_bytes: first.metrics.output_size_bytes,
        ir_size_bytes: first.metrics.ir_size_bytes,
        png_size_bytes: first.metrics.png_size_bytes,
        pages: first.metrics.pages,
        objects: first.metrics.objects,
        objects_per_second: rate(first.metrics.objects, processing_time_ns),
        pages_per_second: rate(first.metrics.pages, processing_time_ns),
    })
}

fn run_worker(executable: &Path, path: &Path) -> TaskResult<WorkerSample> {
    let started = Instant::now();
    let mut child = Command::new(executable)
        .arg("__benchmark-worker")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut peak_memory_bytes = 0;
    loop {
        peak_memory_bytes = peak_memory_bytes.max(read_peak_memory(child.id())?);
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let output = child.wait_with_output()?;
    let wall_time_ns = duration_ns(started.elapsed())?;
    if !output.status.success() {
        return Err(failure(format!(
            "benchmark worker failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if peak_memory_bytes == 0 {
        return Err(failure(format!(
            "peak memory could not be measured for {}",
            path.display()
        )));
    }
    let metrics = serde_json::from_slice(&output.stdout)?;
    Ok(WorkerSample {
        metrics,
        wall_time_ns,
        peak_memory_bytes,
    })
}

#[cfg(target_os = "linux")]
fn read_peak_memory(process_id: u32) -> TaskResult<u64> {
    let path = PathBuf::from(format!("/proc/{process_id}/status"));
    let status = match std::fs::read_to_string(path) {
        Ok(status) => status,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    for field in ["VmHWM:", "VmRSS:"] {
        if let Some(line) = status.lines().find(|line| line.starts_with(field)) {
            let kibibytes = line
                .split_ascii_whitespace()
                .nth(1)
                .ok_or_else(|| failure(format!("invalid {field} value")))?
                .parse::<u64>()?;
            return kibibytes
                .checked_mul(1_024)
                .ok_or_else(|| failure("peak-memory byte count overflow"));
        }
    }
    Ok(0)
}

#[cfg(not(target_os = "linux"))]
fn read_peak_memory(_process_id: u32) -> TaskResult<u64> {
    Err(failure(
        "peak-memory measurement is only supported by the Linux benchmark runner",
    ))
}

fn measure_document(path: &Path) -> TaskResult<WorkerMetrics> {
    let source = DocumentSource::open(path)?;
    match source.format() {
        DocumentFormat::Docx => measure_docx(&source),
        DocumentFormat::Pdf => measure_pdf(&source),
    }
}

fn measure_docx(source: &DocumentSource) -> TaskResult<WorkerMetrics> {
    let parse_started = Instant::now();
    let parsed = parse_docx(source)?;
    let parse_time_ns = duration_ns(parse_started.elapsed())?;
    let layout_started = Instant::now();
    let laid_out = layout_docx(parsed)?;
    let layout_time_ns = duration_ns(layout_started.elapsed())?;
    finish_measurement(
        source,
        laid_out.document,
        parse_time_ns,
        Some(layout_time_ns),
        None,
    )
}

fn measure_pdf(source: &DocumentSource) -> TaskResult<WorkerMetrics> {
    let parse_started = Instant::now();
    let pdf = PdfDocument::open(source)?;
    let parse_time_ns = duration_ns(parse_started.elapsed())?;
    let normalize_started = Instant::now();
    let document = pdf.to_document()?;
    let normalize_time_ns = duration_ns(normalize_started.elapsed())?;
    finish_measurement(
        source,
        document,
        parse_time_ns,
        None,
        Some(normalize_time_ns),
    )
}

fn finish_measurement(
    source: &DocumentSource,
    document: Document,
    parse_time_ns: u64,
    layout_time_ns: Option<u64>,
    normalize_time_ns: Option<u64>,
) -> TaskResult<WorkerMetrics> {
    let render_started = Instant::now();
    let rendered = render_document(
        source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    let render_time_ns = duration_ns(render_started.elapsed())?;
    let ir_size_bytes = u64::try_from(serde_json::to_vec(&document)?.len())?;
    let png_size_bytes = u64::try_from(rendered.png().len())?;
    let output_size_bytes = ir_size_bytes
        .checked_add(png_size_bytes)
        .ok_or_else(|| failure("benchmark output byte count overflow"))?;
    Ok(WorkerMetrics {
        input_size_bytes: source.size_bytes(),
        parse_time_ns,
        layout_time_ns,
        normalize_time_ns,
        render_time_ns,
        output_size_bytes,
        ir_size_bytes,
        png_size_bytes,
        pages: u64::try_from(document.pages.len())?,
        objects: u64::try_from(document.object_ids().len())?,
    })
}

fn budget_violations(
    report: &PerformanceReport,
    budgets: &PerformanceBudgets,
) -> TaskResult<Vec<String>> {
    let reports = report
        .scenarios
        .iter()
        .map(|scenario| (scenario.name.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for budget in &budgets.scenarios {
        let scenario = reports
            .get(budget.name.as_str())
            .ok_or_else(|| failure(format!("missing benchmark report: {}", budget.name)))?;
        check_minimum(
            &mut violations,
            &budget.name,
            "pages",
            scenario.pages as f64,
            budget.minimum_pages as f64,
        );
        check_minimum(
            &mut violations,
            &budget.name,
            "objects",
            scenario.objects as f64,
            budget.minimum_objects as f64,
        );
        check_maximum(
            &mut violations,
            &budget.name,
            "parse_time_ms",
            scenario.parse_time_ms,
            budget.maximum_parse_time_ms as f64,
        );
        let layout_or_normalize = scenario
            .layout_time_ms
            .or(scenario.normalize_time_ms)
            .ok_or_else(|| failure(format!("missing layout or normalize time: {}", budget.name)))?;
        check_maximum(
            &mut violations,
            &budget.name,
            "layout_or_normalize_time_ms",
            layout_or_normalize,
            budget.maximum_layout_or_normalize_time_ms as f64,
        );
        check_maximum(
            &mut violations,
            &budget.name,
            "render_time_ms",
            scenario.render_time_ms,
            budget.maximum_render_time_ms as f64,
        );
        check_maximum(
            &mut violations,
            &budget.name,
            "wall_time_ms",
            scenario.wall_time_ms,
            budget.maximum_wall_time_ms as f64,
        );
        check_maximum(
            &mut violations,
            &budget.name,
            "peak_memory_bytes",
            scenario.peak_memory_bytes as f64,
            budget.maximum_peak_memory_bytes as f64,
        );
        check_maximum(
            &mut violations,
            &budget.name,
            "output_size_bytes",
            scenario.output_size_bytes as f64,
            budget.maximum_output_size_bytes as f64,
        );
        check_minimum(
            &mut violations,
            &budget.name,
            "objects_per_second",
            scenario.objects_per_second,
            budget.minimum_objects_per_second,
        );
        check_minimum(
            &mut violations,
            &budget.name,
            "pages_per_second",
            scenario.pages_per_second,
            budget.minimum_pages_per_second,
        );
    }
    for growth in &budgets.growth_limits {
        let baseline = reports
            .get(growth.baseline.as_str())
            .ok_or_else(|| failure(format!("missing baseline report: {}", growth.baseline)))?;
        let scaled = reports
            .get(growth.scaled.as_str())
            .ok_or_else(|| failure(format!("missing scaled report: {}", growth.scaled)))?;
        let ratio = scaled.peak_memory_bytes as f64 / baseline.peak_memory_bytes as f64;
        check_maximum(
            &mut violations,
            &growth.scaled,
            "peak_memory_growth_ratio",
            ratio,
            growth.maximum_peak_memory_ratio,
        );
    }
    Ok(violations)
}

fn check_minimum(
    violations: &mut Vec<String>,
    scenario: &str,
    metric: &str,
    actual: f64,
    minimum: f64,
) {
    if actual < minimum {
        violations.push(format!(
            "{scenario}.{metric} was {actual:.3}, minimum is {minimum:.3}"
        ));
    }
}

fn check_maximum(
    violations: &mut Vec<String>,
    scenario: &str,
    metric: &str,
    actual: f64,
    maximum: f64,
) {
    if actual > maximum {
        violations.push(format!(
            "{scenario}.{metric} was {actual:.3}, maximum is {maximum:.3}"
        ));
    }
}

fn build_docx(blocks: usize) -> TaskResult<Vec<u8>> {
    let mut document = String::from(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for index in 0..blocks {
        write!(
            document,
            "<w:p><w:r><w:t>Performance block {index:05}</w:t></w:r></w:p>"
        )?;
    }
    document.push_str(
        r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:body></w:document>"#,
    );
    let cursor = io::Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    Ok(writer.finish()?.into_inner())
}

fn build_multipage_pdf(pages: usize) -> TaskResult<Vec<u8>> {
    if pages == 0 {
        return Err(failure("generated PDF page count must be positive"));
    }
    let font_number = pages
        .checked_add(3)
        .ok_or_else(|| failure("generated PDF object count overflow"))?;
    let content_number = pages
        .checked_add(4)
        .ok_or_else(|| failure("generated PDF object count overflow"))?;
    let mut kids = String::new();
    let mut objects = Vec::with_capacity(pages.saturating_add(4));
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    for index in 0..pages {
        write!(kids, "{} 0 R ", index + 3)?;
    }
    objects.push(format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>").into_bytes());
    for _ in 0..pages {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 {font_number} 0 R >> >> /Contents {content_number} 0 R >>"
            )
            .into_bytes(),
        );
    }
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    let content = b"BT /F1 10 Tf 10 20 Td (Performance page) Tj ET";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    objects.push(stream);
    assemble_pdf(objects)
}

fn build_large_image_pdf(width: u32, height: u32) -> TaskResult<Vec<u8>> {
    let image_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| failure("generated image size overflow"))?;
    let image_size = usize::try_from(image_bytes)?;
    let mut image = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {image_size} >>\nstream\n"
    )
    .into_bytes();
    image.resize(
        image
            .len()
            .checked_add(image_size)
            .ok_or_else(|| failure("generated image buffer overflow"))?,
        127,
    );
    image.extend_from_slice(b"\nendstream");
    let content = b"q 200 0 0 100 0 0 cm /Im1 Do Q";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    assemble_pdf(vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        image,
        stream,
    ])
}

fn assemble_pdf(objects: Vec<Vec<u8>>) -> TaskResult<Vec<u8>> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        if offset > 9_999_999_999 {
            return Err(failure("generated PDF xref offset exceeds classic format"));
        }
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(pdf)
}

fn median(values: impl Iterator<Item = u64>) -> TaskResult<u64> {
    let mut values = values.collect::<Vec<_>>();
    if values.is_empty() {
        return Err(failure("cannot compute the median of no samples"));
    }
    values.sort_unstable();
    Ok(values[values.len() / 2])
}

fn median_optional(values: impl Iterator<Item = Option<u64>>) -> TaskResult<Option<u64>> {
    let values = values.collect::<Vec<_>>();
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    if values.iter().any(Option::is_none) {
        return Err(failure("optional benchmark metric was not stable"));
    }
    median(values.into_iter().flatten()).map(Some)
}

fn duration_ns(duration: Duration) -> TaskResult<u64> {
    u64::try_from(duration.as_nanos()).map_err(Into::into)
}

fn milliseconds(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

fn rate(count: u64, nanoseconds: u64) -> f64 {
    if nanoseconds == 0 {
        return count as f64;
    }
    count as f64 * 1_000_000_000.0 / nanoseconds as f64
}

fn failure(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    use super::{
        BUDGET_SCHEMA, PerformanceBudgets, SCENARIOS, build_docx, build_multipage_pdf,
        validate_budgets,
    };
    use docsight_core::DocumentSource;
    use docsight_ooxml::parse_docx;
    use docsight_pdf::PdfDocument;

    #[test]
    fn versioned_budgets_cover_every_scenario() -> Result<(), Box<dyn std::error::Error>> {
        let budgets: PerformanceBudgets =
            serde_json::from_str(include_str!("../../benchmarks/ds9-budgets.json"))?;
        assert_eq!(budgets.schema, BUDGET_SCHEMA);
        validate_budgets(&budgets)?;
        assert_eq!(budgets.scenarios.len(), SCENARIOS.len());
        Ok(())
    }

    #[test]
    fn generated_scale_fixtures_are_parseable() -> Result<(), Box<dyn std::error::Error>> {
        let source = DocumentSource::from_bytes(build_multipage_pdf(1_000)?)?;
        assert_eq!(PdfDocument::open(&source)?.page_count(), 1_000);

        let source = DocumentSource::from_bytes(build_docx(5_000)?)?;
        assert_eq!(parse_docx(&source)?.blocks.len(), 5_000);

        Ok(())
    }
}
