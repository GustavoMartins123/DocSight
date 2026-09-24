use docsight_core::{
    Block, BlockContent, BlockKind, Document, DocumentFormat, DocumentMetadata, DocumentSource,
    IrVersion, LayoutFlags, ObjectId, Page, ParagraphBlock, Rect, Section, SourceSpan,
    TrackedChanges, compute_coverage, validate_canonical,
};
use docsight_diff::diff_semantic;
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_render::{
    RenderRequest, RenderTarget,
    trace::{create_proof_bundle, record_trace, verify_proof_bundle, verify_trace},
};
use docsight_search::{FindMode, FindRequest, find};
use docsight_tables::{RulingSegment, TextSpanItem, detect_tables};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const REPORT_SCHEMA: &str = "docsight.hotspot-report/v1";
const DEFAULT_ITERATIONS: u32 = 3;
const MAX_ITERATIONS: u32 = 9;
const MAX_RESULT_BYTES: usize = 64 * 1024 * 1024;
const DOCX_KEEP_NEXT_SMALL: usize = 1_000;
const DOCX_KEEP_NEXT_LARGE: usize = 10_000;
const CORE_PAGE_SCALE: u32 = 10_000;
const TABLE_RULING_SCALE: usize = 10_000;
const TABLE_SPAN_SCALE: usize = 10_000;
const SHARED_RESOURCE_PAGES: u32 = 1_000;
const SEMANTIC_DIFF_SCALE: usize = 1_000;
const FIND_REPEAT_COUNT: usize = 256;

#[derive(Debug)]
struct HotspotError(String);

impl Display for HotspotError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for HotspotError {}

fn hotspot_error(error: impl Display) -> HotspotError {
    HotspotError(error.to_string())
}

type HotspotResult<T> = Result<T, HotspotError>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Options {
    iterations: u32,
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct HostEnvironment {
    os: &'static str,
    architecture: &'static str,
    profile: &'static str,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    name: &'static str,
    iterations: u32,
    samples_ms: Vec<f64>,
    median_ms: f64,
    minimum_ms: f64,
    maximum_ms: f64,
    result_bytes: u64,
    result_sha256: String,
}

#[derive(Debug, Serialize)]
struct HotspotReport {
    schema: &'static str,
    engine: &'static str,
    host: HostEnvironment,
    cases: Vec<CaseReport>,
}

struct WorkloadMeasurement {
    measured: std::time::Duration,
    result: Vec<u8>,
}

trait Workload {
    fn measure(&mut self) -> HotspotResult<WorkloadMeasurement>;
}

struct FunctionWorkload<F> {
    function: F,
}

impl<F> Workload for FunctionWorkload<F>
where
    F: FnMut() -> HotspotResult<WorkloadMeasurement>,
{
    fn measure(&mut self) -> HotspotResult<WorkloadMeasurement> {
        (self.function)()
    }
}

struct CaseDefinition {
    name: &'static str,
    prepare: fn() -> HotspotResult<Box<dyn Workload>>,
}

const CASES: &[CaseDefinition] = &[
    CaseDefinition {
        name: "canonical_pages_10000",
        prepare: prepare_canonical_pages,
    },
    CaseDefinition {
        name: "coverage_pages_10000",
        prepare: prepare_coverage_pages,
    },
    CaseDefinition {
        name: "docx_keep_next_1000",
        prepare: prepare_keep_next_1000,
    },
    CaseDefinition {
        name: "docx_keep_next_10000",
        prepare: prepare_keep_next_10000,
    },
    CaseDefinition {
        name: "pdf_rulings_10000",
        prepare: prepare_pdf_rulings,
    },
    CaseDefinition {
        name: "pdf_shared_resources_1000",
        prepare: prepare_shared_resources,
    },
    CaseDefinition {
        name: "pdf_trace_and_proof",
        prepare: prepare_trace_and_proof,
    },
    CaseDefinition {
        name: "pdf_unruled_10000",
        prepare: prepare_pdf_unruled,
    },
    CaseDefinition {
        name: "pdf_vector_raster",
        prepare: prepare_vector_raster,
    },
    CaseDefinition {
        name: "semantic_diff_1000",
        prepare: prepare_semantic_diff,
    },
    CaseDefinition {
        name: "unicode_find_512",
        prepare: prepare_unicode_find,
    },
];

pub fn run(arguments: Vec<String>) -> Result<(), Box<dyn Error>> {
    let options = parse_options(arguments)?;
    let mut cases = Vec::with_capacity(CASES.len());
    for (case_index, definition) in CASES.iter().enumerate() {
        eprintln!(
            "hotspots: {}/{} {}",
            case_index + 1,
            CASES.len(),
            definition.name
        );
        let sample_capacity = usize::try_from(options.iterations)
            .map_err(|_| HotspotError("hotspot sample capacity overflow".to_owned()))?;
        let mut samples = Vec::with_capacity(sample_capacity);
        let mut expected: Option<Vec<u8>> = None;
        for _ in 0..options.iterations {
            let mut workload = (definition.prepare)()?;
            let measurement = workload.measure()?;
            if let Some(expected) = &expected
                && expected != &measurement.result
            {
                return Err(Box::new(HotspotError(format!(
                    "{} produced non-identical result bytes",
                    definition.name
                ))));
            }
            if expected.is_none() {
                expected = Some(measurement.result.clone());
            }
            samples.push(duration_ms(measurement.measured));
        }
        let result = expected
            .ok_or_else(|| HotspotError(format!("{} produced no samples", definition.name)))?;
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        cases.push(CaseReport {
            name: definition.name,
            iterations: options.iterations,
            samples_ms: samples,
            median_ms: median(&sorted),
            minimum_ms: sorted.first().copied().unwrap_or_default(),
            maximum_ms: sorted.last().copied().unwrap_or_default(),
            result_bytes: u64::try_from(result.len())
                .map_err(|_| HotspotError("hotspot result length overflow".to_owned()))?,
            result_sha256: sha256_hex(&result),
        });
    }
    let report = HotspotReport {
        schema: REPORT_SCHEMA,
        engine: env!("CARGO_PKG_VERSION"),
        host: HostEnvironment {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            profile: "release",
        },
        cases,
    };
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    if let Some(path) = options.output {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &bytes)?;
    }
    io::stdout().lock().write_all(&bytes)?;
    Ok(())
}

fn parse_options(arguments: Vec<String>) -> HotspotResult<Options> {
    let mut iterations = DEFAULT_ITERATIONS;
    let mut output = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--iterations" => {
                index += 1;
                let raw = arguments.get(index).ok_or_else(|| {
                    HotspotError("--iterations requires a positive integer".to_owned())
                })?;
                iterations = raw.parse::<u32>().map_err(|_| {
                    HotspotError("--iterations requires a positive integer".to_owned())
                })?;
                if !(1..=MAX_ITERATIONS).contains(&iterations) {
                    return Err(HotspotError(format!(
                        "--iterations must be between 1 and {MAX_ITERATIONS}"
                    )));
                }
            }
            "--output" => {
                index += 1;
                let path = arguments
                    .get(index)
                    .ok_or_else(|| HotspotError("--output requires a path".to_owned()))?;
                output = Some(PathBuf::from(path));
            }
            argument => {
                return Err(HotspotError(format!(
                    "unknown hotspot benchmark argument: {argument}"
                )));
            }
        }
        index += 1;
    }
    Ok(Options { iterations, output })
}

fn workload<F>(function: F) -> Box<dyn Workload>
where
    F: FnMut() -> HotspotResult<WorkloadMeasurement> + 'static,
{
    Box::new(FunctionWorkload { function })
}

fn measure<T>(
    operation: impl FnOnce() -> HotspotResult<T>,
    evidence: impl FnOnce(T) -> HotspotResult<Vec<u8>>,
) -> HotspotResult<WorkloadMeasurement> {
    let start = Instant::now();
    let output = operation()?;
    let measured = start.elapsed();
    let result = evidence(output)?;
    if result.len() > MAX_RESULT_BYTES {
        return Err(HotspotError(format!(
            "hotspot result exceeded {} bytes",
            MAX_RESULT_BYTES
        )));
    }
    Ok(WorkloadMeasurement { measured, result })
}

fn prepare_canonical_pages() -> HotspotResult<Box<dyn Workload>> {
    let document = paged_document(CORE_PAGE_SCALE)?;
    Ok(workload(move || {
        measure(
            || validate_canonical(&document).map_err(hotspot_error),
            |_| {
                bounded_json(&serde_json::json!({
                    "blocks": document.blocks.len(),
                    "first_block": document.blocks.first().map(|block| block.id.to_string()),
                    "last_block": document.blocks.last().map(|block| block.id.to_string()),
                    "pages": document.pages.len(),
                    "valid": true
                }))
            },
        )
    }))
}

fn prepare_coverage_pages() -> HotspotResult<Box<dyn Workload>> {
    let (document, source) = paged_document_with_source(CORE_PAGE_SCALE)?;
    Ok(workload(move || {
        measure(
            || compute_coverage(&document, &source, None, false, 1.0).map_err(hotspot_error),
            |report| {
                if report.pages.len()
                    != usize::try_from(CORE_PAGE_SCALE).map_err(|_| {
                        HotspotError("coverage page scale conversion failed".to_owned())
                    })?
                {
                    return Err(HotspotError("coverage page count changed".to_owned()));
                }
                bounded_json(&report)
            },
        )
    }))
}

fn prepare_keep_next_1000() -> HotspotResult<Box<dyn Workload>> {
    prepare_keep_next(DOCX_KEEP_NEXT_SMALL)
}

fn prepare_keep_next_10000() -> HotspotResult<Box<dyn Workload>> {
    prepare_keep_next(DOCX_KEEP_NEXT_LARGE)
}

fn prepare_keep_next(block_count: usize) -> HotspotResult<Box<dyn Workload>> {
    let bytes = build_keep_next_docx(block_count)?;
    let source = DocumentSource::from_bytes(bytes).map_err(hotspot_error)?;
    let document = parse_docx(&source).map_err(hotspot_error)?;
    let expected = block_count
        .checked_add(1)
        .ok_or_else(|| HotspotError("keep-next document length overflow".to_owned()))?;
    if document.blocks.len() != expected {
        return Err(HotspotError(
            "keep-next parser changed block count".to_owned(),
        ));
    }
    let mut next = Some(document);
    Ok(workload(move || {
        let operation = || {
            let input = next
                .take()
                .ok_or_else(|| HotspotError("keep-next workload reused".to_owned()))?;
            layout_docx(input).map_err(hotspot_error)
        };
        measure(operation, |laid_out| {
            if laid_out.document.blocks.len() != document_block_count(block_count) {
                return Err(HotspotError(
                    "keep-next layout changed block count".to_owned(),
                ));
            }
            let keep_count = laid_out
                .document
                .blocks
                .iter()
                .filter(|block| block.flags.keep_with_next)
                .count();
            if keep_count != block_count {
                return Err(HotspotError("keep-next layout changed flags".to_owned()));
            }
            bounded_json(&laid_out.document)
        })
    }))
}

fn document_block_count(block_count: usize) -> usize {
    block_count.saturating_add(1)
}

fn prepare_pdf_rulings() -> HotspotResult<Box<dyn Workload>> {
    let (spans, rulings) = ruled_table_input(TABLE_RULING_SCALE)?;
    Ok(workload(move || {
        measure(
            || Ok(detect_tables(1, &spans, &rulings, 400.0, 400.0)),
            |tables| {
                if tables.len() != 1 || tables[0].cells.len() != 256 {
                    return Err(HotspotError(
                        "ruled table detector changed shape".to_owned(),
                    ));
                }
                bounded_json(&tables)
            },
        )
    }))
}

fn prepare_pdf_unruled() -> HotspotResult<Box<dyn Workload>> {
    let spans = unruled_table_input(TABLE_SPAN_SCALE)?;
    Ok(workload(move || {
        measure(
            || Ok(detect_tables(1, &spans, &[], 1_000.0, 6_020.0)),
            |tables| {
                if tables.len() != 1 || tables[0].cells.len() != TABLE_SPAN_SCALE {
                    return Err(HotspotError(
                        "alignment table detector changed shape".to_owned(),
                    ));
                }
                bounded_json(&tables)
            },
        )
    }))
}

fn prepare_shared_resources() -> HotspotResult<Box<dyn Workload>> {
    let bytes = build_shared_resource_pdf(SHARED_RESOURCE_PAGES)?;
    let expected = usize::try_from(SHARED_RESOURCE_PAGES)
        .map_err(|_| HotspotError("shared-resource page scale conversion failed".to_owned()))?;
    Ok(workload(move || {
        let source = DocumentSource::from_bytes(bytes.clone()).map_err(hotspot_error)?;
        let pdf = PdfDocument::open(&source).map_err(hotspot_error)?;
        measure(
            || pdf.to_document().map_err(hotspot_error),
            |document| {
                if document.pages.len() != expected {
                    return Err(HotspotError(
                        "shared-resource page count changed".to_owned(),
                    ));
                }
                bounded_json(&document)
            },
        )
    }))
}

fn prepare_vector_raster() -> HotspotResult<Box<dyn Workload>> {
    let bytes = build_vector_pdf()?;
    Ok(workload(move || {
        let source = DocumentSource::from_bytes(bytes.clone()).map_err(hotspot_error)?;
        let pdf = PdfDocument::open(&source).map_err(hotspot_error)?;
        measure(
            || pdf.rasterize(1, 72, None).map_err(hotspot_error),
            |raster| {
                if raster.width_px != 220 || raster.height_px != 220 {
                    return Err(HotspotError("vector raster dimensions changed".to_owned()));
                }
                bounded_json(&serde_json::json!({
                    "bbox": raster.bbox,
                    "dpi": raster.dpi,
                    "height_px": raster.height_px,
                    "page": raster.page,
                    "pixel_sha256": sha256_hex(&raster.pixels),
                    "png_sha256": sha256_hex(&raster.png),
                    "warning_codes": raster.warnings.iter().map(|warning| warning.code.clone()).collect::<Vec<_>>(),
                    "width_px": raster.width_px
                }))
            },
        )
    }))
}

fn prepare_trace_and_proof() -> HotspotResult<Box<dyn Workload>> {
    let path = workspace_fixture("synthetic_table.pdf");
    let source = DocumentSource::open(path).map_err(hotspot_error)?;
    let document = PdfDocument::open(&source)
        .and_then(|pdf| pdf.to_document())
        .map_err(hotspot_error)?;
    let object = document
        .tables()
        .next()
        .map(|(block, _)| block.id.to_string())
        .ok_or_else(|| HotspotError("trace fixture has no table".to_owned()))?;
    let request = RenderRequest {
        target: RenderTarget::Object { id: object },
        dpi: 36,
    };
    Ok(workload(move || {
        measure(
            || {
                let trace = record_trace(&source, &request).map_err(hotspot_error)?;
                let trace_bytes = trace.to_bytes().map_err(hotspot_error)?;
                let trace_verification = verify_trace(&trace).map_err(hotspot_error)?;
                let proof_without_crop =
                    create_proof_bundle(&source, &request, false).map_err(hotspot_error)?;
                let proof_without_crop_bytes =
                    proof_without_crop.to_bytes().map_err(hotspot_error)?;
                let proof_without_crop_verification =
                    verify_proof_bundle(&proof_without_crop).map_err(hotspot_error)?;
                let proof_with_crop =
                    create_proof_bundle(&source, &request, true).map_err(hotspot_error)?;
                let proof_with_crop_bytes = proof_with_crop.to_bytes().map_err(hotspot_error)?;
                let proof_with_crop_verification =
                    verify_proof_bundle(&proof_with_crop).map_err(hotspot_error)?;
                Ok((
                    trace_bytes,
                    trace_verification,
                    proof_without_crop_bytes,
                    proof_without_crop_verification,
                    proof_with_crop_bytes,
                    proof_with_crop_verification,
                ))
            },
            |(
                trace_bytes,
                trace_verification,
                proof_without_crop_bytes,
                proof_without_crop_verification,
                proof_with_crop_bytes,
                proof_with_crop_verification,
            )| {
                bounded_json(&serde_json::json!({
                    "proof_with_crop": {
                        "bytes": proof_with_crop_bytes.len(),
                        "sha256": sha256_hex(&proof_with_crop_bytes),
                        "valid": proof_with_crop_verification.valid
                    },
                    "proof_without_crop": {
                        "bytes": proof_without_crop_bytes.len(),
                        "sha256": sha256_hex(&proof_without_crop_bytes),
                        "valid": proof_without_crop_verification.valid
                    },
                    "trace": {
                        "bytes": trace_bytes.len(),
                        "sha256": sha256_hex(&trace_bytes),
                        "valid": trace_verification.valid
                    }
                }))
            },
        )
    }))
}

fn prepare_semantic_diff() -> HotspotResult<Box<dyn Workload>> {
    let before = side_document("before", SEMANTIC_DIFF_SCALE)?;
    let after = side_document("after", SEMANTIC_DIFF_SCALE)?;
    let per_side = u32::try_from(SEMANTIC_DIFF_SCALE)
        .map_err(|_| HotspotError("semantic diff scale conversion failed".to_owned()))?;
    let expected = per_side
        .checked_add(per_side)
        .ok_or_else(|| HotspotError("semantic diff total overflow".to_owned()))?;
    Ok(workload(move || {
        measure(
            || diff_semantic(&before, &after).map_err(hotspot_error),
            |diff| {
                if diff.total_changes != expected
                    || diff.paragraphs.added != per_side
                    || diff.paragraphs.removed != per_side
                    || !diff.lineage.is_empty()
                {
                    return Err(HotspotError(
                        "semantic diff changed rejected-pair shape".to_owned(),
                    ));
                }
                bounded_json(&diff)
            },
        )
    }))
}

fn prepare_unicode_find() -> HotspotResult<Box<dyn Workload>> {
    let unit = format!("İ·i\u{307}·{}", "Λ".repeat(240));
    let text = unit.repeat(FIND_REPEAT_COUNT);
    let document = text_document(&text)?;
    let request = FindRequest {
        pattern: "i\u{307}".to_owned(),
        mode: FindMode::Literal,
        ignore_case: true,
        kinds: Default::default(),
        pages: None,
        region: None,
    };
    let expected = FIND_REPEAT_COUNT
        .checked_mul(2)
        .ok_or_else(|| HotspotError("Unicode find match count overflow".to_owned()))?;
    Ok(workload(move || {
        measure(
            || find(&document, &request).map_err(hotspot_error),
            |result| {
                if result.total_matches != expected || result.matches.len() != expected {
                    return Err(HotspotError("Unicode find match count changed".to_owned()));
                }
                bounded_json(&result)
            },
        )
    }))
}

fn paged_document(page_count: u32) -> HotspotResult<Document> {
    Ok(paged_document_with_source(page_count)?.0)
}

fn paged_document_with_source(page_count: u32) -> HotspotResult<(Document, DocumentSource)> {
    let digest = "1".repeat(64);
    let capacity = usize::try_from(page_count)
        .map_err(|_| HotspotError("paged document capacity overflow".to_owned()))?;
    let mut blocks = Vec::with_capacity(capacity);
    let mut pages = Vec::with_capacity(capacity);
    for page in 1..=page_count {
        let path = format!("/body/p[{page}]");
        let id = ObjectId::new("p", &digest, &path);
        blocks.push(paragraph_block(
            id.clone(),
            page,
            page,
            50.0,
            &path,
            &format!("page {page}"),
        )?);
        pages.push(Page {
            number: page,
            width_pt: 595.0,
            height_pt: 842.0,
            section_index: Some(1),
            block_ids: vec![id],
            continued_block_ids: Vec::new(),
            overlays: Vec::new(),
        });
    }
    let last_block_id = blocks
        .last()
        .map(|block| block.id.clone())
        .ok_or_else(|| HotspotError("paged document has no blocks".to_owned()))?;
    let document = Document {
        version: IrVersion::current(),
        id: "doc_1234567890ab".to_owned(),
        sha256: digest,
        format: DocumentFormat::Docx,
        size_bytes: 1,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections: vec![Section {
            id: ObjectId::new("sec", &"1".repeat(64), "/body/sect[1]"),
            section_index: 1,
            page_width_pt: Some(595.0),
            page_height_pt: Some(842.0),
            margin_top_pt: Some(72.0),
            margin_right_pt: Some(72.0),
            margin_bottom_pt: Some(72.0),
            margin_left_pt: Some(72.0),
            header_text: None,
            footer_text: None,
            start: Default::default(),
            title_page: false,
            even_and_odd_headers: false,
            header_distance_pt: None,
            footer_distance_pt: None,
            columns: 1,
            page_number_start: None,
            page_number_format: None,
            headers_footers: Vec::new(),
            last_block_id: Some(last_block_id),
        }],
        pages,
        blocks,
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: TrackedChanges::default(),
        warnings: Vec::new(),
    };
    let source =
        DocumentSource::open(workspace_fixture("sample_headings.docx")).map_err(hotspot_error)?;
    Ok((document, source))
}

fn side_document(side: &str, count: usize) -> HotspotResult<Document> {
    let digest = if side == "before" {
        "2".repeat(64)
    } else {
        "3".repeat(64)
    };
    let mut blocks = Vec::with_capacity(count);
    for index in 0..count {
        let path = format!("/{side}/p[{}]", index + 1);
        blocks.push(paragraph_block(
            ObjectId::new("p", &digest, &path),
            1,
            u32::try_from(index + 1)
                .map_err(|_| HotspotError("side document order overflow".to_owned()))?,
            0.0,
            &path,
            &format!("{side}-{index:05}"),
        )?);
    }
    let last_block_id = blocks
        .last()
        .map(|block| block.id.clone())
        .ok_or_else(|| HotspotError("side document has no blocks".to_owned()))?;
    Ok(Document {
        version: IrVersion::current(),
        id: format!("doc_{}", side),
        sha256: digest.clone(),
        format: DocumentFormat::Docx,
        size_bytes: 1,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections: vec![Section {
            id: ObjectId::new("sec", &digest, &format!("/{side}/sect[1]")),
            section_index: 1,
            page_width_pt: Some(595.0),
            page_height_pt: Some(842.0),
            margin_top_pt: Some(72.0),
            margin_right_pt: Some(72.0),
            margin_bottom_pt: Some(72.0),
            margin_left_pt: Some(72.0),
            header_text: None,
            footer_text: None,
            start: Default::default(),
            title_page: false,
            even_and_odd_headers: false,
            header_distance_pt: None,
            footer_distance_pt: None,
            columns: 1,
            page_number_start: None,
            page_number_format: None,
            headers_footers: Vec::new(),
            last_block_id: Some(last_block_id),
        }],
        pages: vec![Page {
            number: 1,
            width_pt: 595.0,
            height_pt: 842.0,
            section_index: Some(1),
            block_ids: blocks.iter().map(|block| block.id.clone()).collect(),
            continued_block_ids: Vec::new(),
            overlays: Vec::new(),
        }],
        blocks,
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: TrackedChanges::default(),
        warnings: Vec::new(),
    })
}

fn text_document(text: &str) -> HotspotResult<Document> {
    let digest = "4".repeat(64);
    let id = ObjectId::new("p", &digest, "/body/p[1]");
    let block = paragraph_block(id.clone(), 1, 1, 50.0, "/body/p[1]", text)?;
    Ok(Document {
        version: IrVersion::current(),
        id: "doc_1234567890ab".to_owned(),
        sha256: digest.clone(),
        format: DocumentFormat::Docx,
        size_bytes: u64::try_from(text.len())
            .map_err(|_| HotspotError("text document size overflow".to_owned()))?,
        metadata: DocumentMetadata::default(),
        styles: Vec::new(),
        sections: vec![Section {
            id: ObjectId::new("sec", &digest, "/body/sect[1]"),
            section_index: 1,
            page_width_pt: Some(595.0),
            page_height_pt: Some(842.0),
            margin_top_pt: Some(72.0),
            margin_right_pt: Some(72.0),
            margin_bottom_pt: Some(72.0),
            margin_left_pt: Some(72.0),
            header_text: None,
            footer_text: None,
            start: Default::default(),
            title_page: false,
            even_and_odd_headers: false,
            header_distance_pt: None,
            footer_distance_pt: None,
            columns: 1,
            page_number_start: None,
            page_number_format: None,
            headers_footers: Vec::new(),
            last_block_id: Some(id.clone()),
        }],
        pages: vec![Page {
            number: 1,
            width_pt: 595.0,
            height_pt: 842.0,
            section_index: Some(1),
            block_ids: vec![id],
            continued_block_ids: Vec::new(),
            overlays: Vec::new(),
        }],
        blocks: vec![block],
        resources: Vec::new(),
        links: Vec::new(),
        comments: Vec::new(),
        tracked_changes: TrackedChanges::default(),
        warnings: Vec::new(),
    })
}

fn paragraph_block(
    id: ObjectId,
    page: u32,
    reading_order: u32,
    y0: f32,
    path: &str,
    text: &str,
) -> HotspotResult<Block> {
    Ok(Block {
        id,
        kind: BlockKind::Paragraph,
        page: Some(page),
        bbox: Some(Rect::new(50.0, y0, 545.0, y0 + 12.0).map_err(hotspot_error)?),
        z_index: 0,
        reading_order,
        source: SourceSpan::new(path),
        confidence: 1.0,
        flags: LayoutFlags::default(),
        format: Default::default(),
        continuations: Vec::new(),
        content: BlockContent::Paragraph(ParagraphBlock {
            text: text.to_owned(),
            style_id: None,
        }),
    })
}

fn coordinate(value: u32) -> HotspotResult<f32> {
    let value = u16::try_from(value)
        .map_err(|_| HotspotError("benchmark coordinate overflow".to_owned()))?;
    Ok(f32::from(value))
}

fn ruled_table_input(
    target_rulings: usize,
) -> HotspotResult<(Vec<TextSpanItem>, Vec<RulingSegment>)> {
    let rows = 16_u32;
    let columns = 16_u32;
    let mut rulings = Vec::with_capacity(target_rulings);
    for row in 0..=rows {
        let y = 20.0 + coordinate(row)? * 22.5;
        rulings.push(RulingSegment {
            x0: 20.0,
            y0: y,
            x1: 380.0,
            y1: y,
        });
    }
    for column in 0..=columns {
        let x = 20.0 + coordinate(column)? * 22.5;
        rulings.push(RulingSegment {
            x0: x,
            y0: 20.0,
            x1: x,
            y1: 380.0,
        });
    }
    while rulings.len() < target_rulings {
        let index = rulings.len() % 34;
        rulings.push(rulings[index].clone());
    }
    if rulings.len() != target_rulings {
        return Err(HotspotError("ruling scale construction failed".to_owned()));
    }
    let span_count = usize::try_from(
        rows.checked_mul(columns)
            .ok_or_else(|| HotspotError("ruled table cell count overflow".to_owned()))?,
    )
    .map_err(|_| HotspotError("ruled table cell count conversion failed".to_owned()))?;
    let mut spans = Vec::with_capacity(span_count);
    for row in 0..rows {
        for column in 0..columns {
            let x0 = 22.5 + coordinate(column)? * 22.5;
            let y0 = 22.5 + coordinate(row)? * 22.5;
            spans.push(TextSpanItem {
                text: format!("r{row:02}c{column:02}"),
                bbox: Rect::new(x0, y0, x0 + 18.0, y0 + 10.0).map_err(hotspot_error)?,
                font_size: 6.0,
                bold: false,
            });
        }
    }
    Ok((spans, rulings))
}

fn unruled_table_input(target_spans: usize) -> HotspotResult<Vec<TextSpanItem>> {
    let columns = 20_u32;
    let column_count = usize::try_from(columns)
        .map_err(|_| HotspotError("unruled table column conversion failed".to_owned()))?;
    let row_count = target_spans / column_count;
    let rows = u32::try_from(row_count)
        .map_err(|_| HotspotError("unruled table scale conversion failed".to_owned()))?;
    if row_count.checked_mul(column_count) != Some(target_spans) {
        return Err(HotspotError(
            "unruled table scale is not rectangular".to_owned(),
        ));
    }
    let mut spans = Vec::with_capacity(target_spans);
    for row in 0..rows {
        for column in 0..columns {
            let x0 = 10.0 + coordinate(column)? * 50.0;
            let y0 = 10.0 + coordinate(row)? * 12.0;
            spans.push(TextSpanItem {
                text: format!("r{row:03}c{column:02}"),
                bbox: Rect::new(x0, y0, x0 + 42.0, y0 + 8.0).map_err(hotspot_error)?,
                font_size: 8.0,
                bold: false,
            });
        }
    }
    Ok(spans)
}

fn build_keep_next_docx(block_count: usize) -> HotspotResult<Vec<u8>> {
    let capacity = block_count
        .checked_mul(96)
        .ok_or_else(|| HotspotError("keep-next XML capacity overflow".to_owned()))?;
    let mut body = String::with_capacity(capacity);
    for index in 0..block_count {
        body.push_str("<w:p><w:pPr><w:keepNext/></w:pPr><w:r><w:t>");
        body.push_str(&format!("Keep chain {index:05}"));
        body.push_str("</w:t></w:r></w:p>");
    }
    body.push_str("<w:p><w:r><w:t>Chain terminator</w:t></w:r></w:p>");
    let document_xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\"/></w:sectPr></w:body></w:document>"
    );
    build_docx(document_xml.as_bytes())
}

fn build_docx(document_xml: &[u8]) -> HotspotResult<Vec<u8>> {
    let cursor = io::Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    zip.start_file("[Content_Types].xml", options)
        .map_err(hotspot_error)?;
    zip.write_all(b"<Types/>").map_err(hotspot_error)?;
    zip.start_file("word/document.xml", options)
        .map_err(hotspot_error)?;
    zip.write_all(document_xml).map_err(hotspot_error)?;
    zip.finish()
        .map(|cursor| cursor.into_inner())
        .map_err(hotspot_error)
}

fn build_shared_resource_pdf(page_count: u32) -> HotspotResult<Vec<u8>> {
    let page_object_start = 4_u32;
    let content_object_start = page_object_start
        .checked_add(page_count)
        .ok_or_else(|| HotspotError("shared PDF object overflow".to_owned()))?;
    let mut kids = String::new();
    for page in 0..page_count {
        kids.push_str(&format!("{} 0 R ", page_object_start + page));
    }
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>").into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for page in 0..page_count {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                content_object_start + page
            )
            .into_bytes(),
        );
    }
    for page in 0..page_count {
        let content = format!("BT /F1 10 Tf 10 80 Td (P{page:04}) Tj ET");
        objects.push(stream_object(content.as_bytes()));
    }
    assemble_pdf(objects)
}

fn build_vector_pdf() -> HotspotResult<Vec<u8>> {
    let mut content = String::from("q 10 10 180 180 re W n 20 20 160 160 re W* n ");
    for index in 0..128_u32 {
        let offset = coordinate(index % 16)? * 8.0;
        content.push_str(&format!(
            "{} {} m {} {} {} {} {} {} c B Q q 10 10 180 180 re W n ",
            20.0 + offset,
            20.0 + offset,
            40.0 + offset,
            5.0 + offset,
            80.0 + offset,
            60.0 + offset,
            120.0 + offset,
            30.0 + offset
        ));
    }
    content.push_str("BT /F1 8 Tf 20 100 Td ");
    for _ in 0..2_048 {
        content.push_str("(A) Tj 1 0 Td ");
    }
    content.push_str("ET Q");
    assemble_pdf(vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 220 220] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream_object(content.as_bytes()),
    ])
}

fn stream_object(content: &[u8]) -> Vec<u8> {
    let mut object = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    object.extend_from_slice(content);
    object.extend_from_slice(b"\nendstream");
    object
}

fn assemble_pdf(objects: Vec<Vec<u8>>) -> HotspotResult<Vec<u8>> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        let number = u32::try_from(index + 1)
            .map_err(|_| HotspotError("PDF object number overflow".to_owned()))?;
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    let size = u32::try_from(objects.len() + 1)
        .map_err(|_| HotspotError("PDF object count overflow".to_owned()))?;
    pdf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    Ok(pdf)
}

fn workspace_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn bounded_json(value: &impl Serialize) -> HotspotResult<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(hotspot_error)?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(HotspotError(format!(
            "hotspot result exceeded {} bytes",
            MAX_RESULT_BYTES
        )));
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn duration_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_registry_is_exact_sorted_and_unique() {
        let names = CASES.iter().map(|case| case.name).collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), 11);
    }

    #[test]
    fn hotspot_options_accept_only_bounded_output_and_iterations() {
        assert_eq!(
            parse_options(Vec::new()).map_err(|error| error.to_string()),
            Ok(Options {
                iterations: DEFAULT_ITERATIONS,
                output: None
            })
        );
        assert!(parse_options(vec!["--iterations".to_owned(), "0".to_owned()]).is_err());
        assert!(parse_options(vec!["--unknown".to_owned()]).is_err());
        assert!(parse_options(vec!["--output".to_owned()]).is_err());
    }

    #[test]
    fn generated_benchmark_inputs_are_structurally_valid() -> Result<(), Box<dyn std::error::Error>>
    {
        let document = paged_document(3)?;
        validate_canonical(&document)?;
        let source = DocumentSource::from_bytes(build_keep_next_docx(16)?)?;
        let parsed = parse_docx(&source)?;
        let laid_out = layout_docx(parsed)?;
        assert_eq!(laid_out.document.blocks.len(), 17);
        let (spans, rulings) = ruled_table_input(100)?;
        assert_eq!(rulings.len(), 100);
        assert_eq!(detect_tables(1, &spans, &rulings, 400.0, 400.0).len(), 1);
        Ok(())
    }
}
