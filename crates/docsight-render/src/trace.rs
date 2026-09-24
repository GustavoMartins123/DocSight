use crate::{RenderRequest, RenderTarget, document_glyph_coverage, render_document_with_password};
use docsight_core::{
    BlockKind, Diagnostic, DiagnosticSeverity, DocsightError, Document, DocumentFormat,
    DocumentSource, EvidenceRecord, ObjectId, Rect, ResourceKind, compute_evidence, write_all,
};
use docsight_ingest::{ingest_docx, ingest_with_password as load_document};
use docsight_layout::LaidOutPage;
use docsight_pdf::{
    PdfDocument, PdfTraceClip, PdfTraceDisplayOperation, PdfTraceFillRule, PdfTraceLineCap,
    PdfTraceLineJoin, PdfTracePage, PdfTracePathSegment, PdfTraceResourceKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const TRACE_SCHEMA: &str = "docsight.trace/v2";
const PROOF_BUNDLE_SCHEMA: &str = "docsight.proof-bundle/v2";
const MAX_ARTIFACT_MANIFEST_BYTES: u64 = 1_048_576;
const MAX_ARTIFACT_CROP_BYTES: u64 = docsight_core::MAX_RASTER_OUTPUT_BYTES;
const MAX_ARCHIVE_OVERHEAD_BYTES: u64 = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactLimits {
    pub max_document_bytes: u64,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: docsight_core::MAX_INSPECT_BYTES,
        }
    }
}

fn checked_limit(value: u64, additions: &[u64], resource: &str) -> Result<u64, DocsightError> {
    additions.iter().try_fold(value, |total, addition| {
        total
            .checked_add(*addition)
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: resource.to_owned(),
                limit: u64::MAX,
            })
    })
}

fn trace_archive_limit(limits: ArtifactLimits) -> Result<u64, DocsightError> {
    checked_limit(
        limits.max_document_bytes,
        &[MAX_ARTIFACT_MANIFEST_BYTES, MAX_ARCHIVE_OVERHEAD_BYTES],
        "trace archive byte limit",
    )
}

fn proof_archive_limit(limits: ArtifactLimits) -> Result<u64, DocsightError> {
    checked_limit(
        limits.max_document_bytes,
        &[
            MAX_ARTIFACT_MANIFEST_BYTES,
            MAX_ARTIFACT_CROP_BYTES,
            MAX_ARCHIVE_OVERHEAD_BYTES,
        ],
        "proof archive byte limit",
    )
}

fn validate_document_size(
    actual: u64,
    limits: ArtifactLimits,
    resource: &str,
) -> Result<(), DocsightError> {
    if actual > limits.max_document_bytes {
        return Err(DocsightError::ResourceLimit {
            resource: resource.to_owned(),
            limit: limits.max_document_bytes,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TraceSelector {
    Page { page: u32 },
    Region { page: u32, bbox: Rect },
    Object { id: String },
}

impl TraceSelector {
    fn from_render_target(target: &RenderTarget) -> Self {
        match target {
            RenderTarget::Page { page } => Self::Page { page: *page },
            RenderTarget::Region { page, bbox } => Self::Region {
                page: *page,
                bbox: *bbox,
            },
            RenderTarget::Object { id } => Self::Object { id: id.clone() },
        }
    }

    fn to_render_target(&self) -> RenderTarget {
        match self {
            Self::Page { page } => RenderTarget::Page { page: *page },
            Self::Region { page, bbox } => RenderTarget::Region {
                page: *page,
                bbox: *bbox,
            },
            Self::Object { id } => RenderTarget::Object { id: id.clone() },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTarget {
    pub selector: TraceSelector,
    pub page: u32,
    pub bbox: Rect,
    pub dpi: u16,
}

impl TraceTarget {
    fn to_request(&self) -> RenderRequest {
        RenderRequest {
            target: self.selector.to_render_target(),
            dpi: self.dpi,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSource {
    pub document_id: String,
    pub document_sha256: String,
    pub format: DocumentFormat,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReproductionFingerprint {
    pub engine: String,
    pub ooxml_engine: String,
    pub pdf_engine: String,
    pub raster_engine: String,
    pub fonts: String,
    pub layout_profile: String,
    pub result_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceResource {
    pub id: String,
    pub kind: TraceResourceKind,
    pub name: String,
    pub target: String,
    pub content_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceResourceKind {
    Font,
    Image,
    Relationship,
    EmbeddedObject,
    GraphicsState,
    XObject,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceGlyphRun {
    pub text_sha256: String,
    pub bbox: Rect,
    pub font_size_pt: f32,
    pub font_name: String,
    pub bold: bool,
    pub color_argb: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TraceDisplayOperation {
    Text {
        text_sha256: String,
        bbox: Rect,
        font_size_pt: f32,
        font_name: String,
        bold: bool,
        color_argb: u32,
        stroke_color_argb: u32,
        stroke_width_pt: f32,
        render_mode: u8,
        mirrored_x: bool,
        clips: Vec<PdfTraceClip>,
    },
    Border {
        bbox: Rect,
        color_argb: u32,
    },
    Fill {
        path: Vec<PdfTracePathSegment>,
        color_argb: u32,
        fill_rule: PdfTraceFillRule,
        clips: Vec<PdfTraceClip>,
    },
    Stroke {
        path: Vec<PdfTracePathSegment>,
        color_argb: u32,
        width_pt: f32,
        line_cap: PdfTraceLineCap,
        line_join: PdfTraceLineJoin,
        miter_limit: f32,
        dash_pattern_pt: Vec<f32>,
        dash_phase_pt: f32,
        clips: Vec<PdfTraceClip>,
    },
    Figure {
        bbox: Rect,
        resource_name: String,
        clips: Vec<PdfTraceClip>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceDisplayList {
    pub page: u32,
    pub page_bbox: Rect,
    pub operations: Vec<TraceDisplayOperation>,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTableSizing {
    pub object_id: String,
    pub page: u32,
    pub bbox: Option<Rect>,
    pub rows: u32,
    pub columns: u32,
    pub column_widths_pt: Option<Vec<f32>>,
    pub detector: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracePaginationDecision {
    pub object_id: String,
    pub kind: BlockKind,
    pub page: u32,
    pub bbox: Option<Rect>,
    pub reading_order: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceDecisionCoverage {
    pub resolved_resources: TraceDecisionStatus,
    pub glyph_metrics: TraceDecisionStatus,
    pub line_breaks: TraceDecisionStatus,
    pub table_sizing: TraceDecisionStatus,
    pub pagination: TraceDecisionStatus,
    pub display_list_operations: TraceDecisionStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceDecisionStatus {
    Verified,
    NotApplicable,
    Unavailable,
}

impl TraceDecisionCoverage {
    fn unavailable_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.resolved_resources == TraceDecisionStatus::Unavailable {
            names.push("resolved_resources");
        }
        if self.glyph_metrics == TraceDecisionStatus::Unavailable {
            names.push("glyph_metrics");
        }
        if self.line_breaks == TraceDecisionStatus::Unavailable {
            names.push("line_breaks");
        }
        if self.table_sizing == TraceDecisionStatus::Unavailable {
            names.push("table_sizing");
        }
        if self.pagination == TraceDecisionStatus::Unavailable {
            names.push("pagination");
        }
        if self.display_list_operations == TraceDecisionStatus::Unavailable {
            names.push("display_list_operations");
        }
        names
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceRaster {
    pub page: u32,
    pub dpi: u16,
    pub bbox: Rect,
    pub width_px: u32,
    pub height_px: u32,
    pub media_type: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceManifest {
    pub schema: String,
    pub source: TraceSource,
    pub fingerprint: ReproductionFingerprint,
    pub target: TraceTarget,
    pub decision_coverage: TraceDecisionCoverage,
    pub resources: Vec<TraceResource>,
    pub glyph_runs: Vec<TraceGlyphRun>,
    pub table_sizing: Vec<TraceTableSizing>,
    pub pagination: Vec<TracePaginationDecision>,
    pub display_list: TraceDisplayList,
    pub raster: TraceRaster,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceArtifact {
    pub manifest: TraceManifest,
    source_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArtifactWriteResult {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplayVerification {
    pub schema: &'static str,
    pub valid: bool,
    pub trace_sha256: String,
    pub document_sha256: String,
    pub display_list_sha256: String,
    pub raster_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofCrop {
    pub sha256: String,
    pub bytes: u64,
    pub media_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofBundleManifest {
    pub schema: String,
    pub trace: TraceManifest,
    pub evidence: Vec<EvidenceRecord>,
    pub crop: Option<ProofCrop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProofBundle {
    pub manifest: ProofBundleManifest,
    source_bytes: Vec<u8>,
    crop_png: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProofVerification {
    pub schema: &'static str,
    pub valid: bool,
    pub bundle_sha256: String,
    pub document_sha256: String,
    pub evidence_count: usize,
    pub trace_verified: bool,
    pub crop_verified: bool,
}

pub fn record_trace(
    source: &DocumentSource,
    request: &RenderRequest,
) -> Result<TraceArtifact, DocsightError> {
    record_trace_with_password(source, request, b"", ArtifactLimits::default())
}

pub fn record_trace_with_password(
    source: &DocumentSource,
    request: &RenderRequest,
    password: &[u8],
    limits: ArtifactLimits,
) -> Result<TraceArtifact, DocsightError> {
    validate_document_size(source.size_bytes(), limits, "trace source bytes")?;
    let rendered = render_document_with_password(source, request, password)?;
    let selector = TraceSelector::from_render_target(&request.target);
    let target = TraceTarget {
        selector,
        page: rendered.metadata.page,
        bbox: rendered.metadata.bbox,
        dpi: rendered.metadata.dpi,
    };
    let material = trace_material(source, rendered.metadata.page, password)?;
    let mut warnings = material.document().warnings.clone();
    for warning in &rendered.warnings {
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
    let (glyph_runs, operations, decision_coverage) =
        material.trace_operations(rendered.metadata.page);
    let page_bbox = material.page_bbox(rendered.metadata.page)?;
    let display_list = TraceDisplayList {
        page: rendered.metadata.page,
        page_bbox,
        operations,
        sha256: String::new(),
    };
    let mut display_list = display_list;
    display_list.sha256 = canonical_sha256(&display_list)?;
    let unavailable = decision_coverage.unavailable_names();
    if !unavailable.is_empty() {
        warnings.push(Diagnostic {
            code: "TRACE_DECISION_PARTIAL".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "trace does not expose complete decision data for {}",
                unavailable.join(", ")
            ),
            effect:
                "replay verifies the available normalized decisions and raster fingerprint only"
                    .to_owned(),
            object: None,
            page: Some(rendered.metadata.page),
            occurrences: None,
        });
    }
    let raster_bytes = checked_len(rendered.png().len(), "raster artifact bytes")?;
    let manifest = TraceManifest {
        schema: TRACE_SCHEMA.to_owned(),
        source: TraceSource {
            document_id: source.id(),
            document_sha256: source.sha256().to_owned(),
            format: source.format(),
            bytes: source.size_bytes(),
        },
        fingerprint: reproduction_fingerprint(source),
        target,
        decision_coverage,
        resources: trace_resources(&material, source),
        glyph_runs,
        table_sizing: material.trace_table_sizing(rendered.metadata.page),
        pagination: material.trace_pagination(rendered.metadata.page),
        display_list,
        raster: TraceRaster {
            page: rendered.metadata.page,
            dpi: rendered.metadata.dpi,
            bbox: rendered.metadata.bbox,
            width_px: rendered.metadata.width_px,
            height_px: rendered.metadata.height_px,
            media_type: rendered.metadata.media_type.to_owned(),
            sha256: sha256_hex(rendered.png()),
            bytes: raster_bytes,
        },
        warnings,
    };
    Ok(TraceArtifact {
        manifest,
        source_bytes: source.bytes().to_vec(),
    })
}

pub fn create_proof_bundle(
    source: &DocumentSource,
    request: &RenderRequest,
    include_crop: bool,
) -> Result<ProofBundle, DocsightError> {
    create_proof_bundle_with_password(
        source,
        request,
        include_crop,
        b"",
        ArtifactLimits::default(),
    )
}

pub fn create_proof_bundle_with_password(
    source: &DocumentSource,
    request: &RenderRequest,
    include_crop: bool,
    password: &[u8],
    limits: ArtifactLimits,
) -> Result<ProofBundle, DocsightError> {
    let trace = record_trace_with_password(source, request, password, limits)?;
    let document = load_document(source, password)?;
    let glyph_coverage = document_glyph_coverage(&document, source);
    let evidence = selected_evidence(
        &document,
        source,
        &trace.manifest.target,
        trace.manifest.raster.sha256.clone(),
        glyph_coverage,
    )?;
    let crop_png = if include_crop {
        let rendered = render_document_with_password(source, request, password)?;
        let actual = sha256_hex(rendered.png());
        if actual != trace.manifest.raster.sha256 {
            return Err(DocsightError::VerificationFailed {
                message: "proof crop did not match its trace raster fingerprint".to_owned(),
            });
        }
        Some(rendered.png().to_vec())
    } else {
        None
    };
    let crop = crop_png
        .as_ref()
        .map(|bytes| {
            Ok(ProofCrop {
                sha256: sha256_hex(bytes),
                bytes: checked_len(bytes.len(), "proof crop bytes")?,
                media_type: "image/png".to_owned(),
            })
        })
        .transpose()?;
    Ok(ProofBundle {
        manifest: ProofBundleManifest {
            schema: PROOF_BUNDLE_SCHEMA.to_owned(),
            trace: trace.manifest,
            evidence,
            crop,
        },
        source_bytes: trace.source_bytes,
        crop_png,
    })
}

pub fn read_trace(path: &Path) -> Result<TraceArtifact, DocsightError> {
    read_trace_with_limits(path, ArtifactLimits::default())
}

pub fn read_trace_with_limits(
    path: &Path,
    limits: ArtifactLimits,
) -> Result<TraceArtifact, DocsightError> {
    let maximum = trace_archive_limit(limits)?;
    let bytes = read_artifact(path, maximum, "trace artifact")?;
    TraceArtifact::from_bytes_with_limits(&bytes, limits)
}

pub fn read_proof_bundle(path: &Path) -> Result<ProofBundle, DocsightError> {
    read_proof_bundle_with_limits(path, ArtifactLimits::default())
}

pub fn read_proof_bundle_with_limits(
    path: &Path,
    limits: ArtifactLimits,
) -> Result<ProofBundle, DocsightError> {
    let maximum = proof_archive_limit(limits)?;
    let bytes = read_artifact(path, maximum, "proof bundle")?;
    ProofBundle::from_bytes_with_limits(&bytes, limits)
}

pub fn verify_trace(trace: &TraceArtifact) -> Result<ReplayVerification, DocsightError> {
    verify_trace_with_password(trace, b"", ArtifactLimits::default())
}

pub fn verify_trace_with_password(
    trace: &TraceArtifact,
    password: &[u8],
    limits: ArtifactLimits,
) -> Result<ReplayVerification, DocsightError> {
    validate_trace_manifest(&trace.manifest)?;
    let source = DocumentSource::from_bytes(trace.source_bytes.clone())?;
    let reproduced = record_trace_with_password(
        &source,
        &trace.manifest.target.to_request(),
        password,
        limits,
    )?;
    if trace.manifest != reproduced.manifest {
        return Err(DocsightError::VerificationFailed {
            message: "trace manifest differs from deterministic replay".to_owned(),
        });
    }
    let trace_bytes = trace.to_bytes_with_limits(limits)?;
    Ok(ReplayVerification {
        schema: TRACE_SCHEMA,
        valid: true,
        trace_sha256: sha256_hex(&trace_bytes),
        document_sha256: trace.manifest.source.document_sha256.clone(),
        display_list_sha256: trace.manifest.display_list.sha256.clone(),
        raster_sha256: trace.manifest.raster.sha256.clone(),
    })
}

pub fn verify_proof_bundle(bundle: &ProofBundle) -> Result<ProofVerification, DocsightError> {
    verify_proof_bundle_with_password(bundle, b"", ArtifactLimits::default())
}

pub fn verify_proof_bundle_with_password(
    bundle: &ProofBundle,
    password: &[u8],
    limits: ArtifactLimits,
) -> Result<ProofVerification, DocsightError> {
    validate_proof_manifest(&bundle.manifest)?;
    let source = DocumentSource::from_bytes(bundle.source_bytes.clone())?;
    let reproduced = create_proof_bundle_with_password(
        &source,
        &bundle.manifest.trace.target.to_request(),
        bundle.crop_png.is_some(),
        password,
        limits,
    )?;
    if bundle.manifest != reproduced.manifest {
        return Err(DocsightError::VerificationFailed {
            message: "proof bundle manifest differs from deterministic verification".to_owned(),
        });
    }
    if bundle.crop_png != reproduced.crop_png {
        return Err(DocsightError::VerificationFailed {
            message: "proof bundle crop differs from deterministic verification".to_owned(),
        });
    }
    let bundle_bytes = bundle.to_bytes_with_limits(limits)?;
    Ok(ProofVerification {
        schema: PROOF_BUNDLE_SCHEMA,
        valid: true,
        bundle_sha256: sha256_hex(&bundle_bytes),
        document_sha256: bundle.manifest.trace.source.document_sha256.clone(),
        evidence_count: bundle.manifest.evidence.len(),
        trace_verified: true,
        crop_verified: bundle.crop_png.is_some(),
    })
}

impl TraceArtifact {
    pub fn document_source(&self) -> Result<DocumentSource, DocsightError> {
        DocumentSource::from_bytes(self.source_bytes.clone())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, DocsightError> {
        self.to_bytes_with_limits(ArtifactLimits::default())
    }

    pub fn to_bytes_with_limits(&self, limits: ArtifactLimits) -> Result<Vec<u8>, DocsightError> {
        validate_trace_manifest(&self.manifest)?;
        let source_bytes = checked_len(self.source_bytes.len(), "trace source bytes")?;
        validate_document_size(source_bytes, limits, "trace source bytes")?;
        let manifest = canonical_json(&self.manifest)?;
        let manifest_bytes = checked_len(manifest.len(), "trace manifest bytes")?;
        if manifest_bytes > MAX_ARTIFACT_MANIFEST_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "trace manifest bytes".to_owned(),
                limit: MAX_ARTIFACT_MANIFEST_BYTES,
            });
        }
        let bytes = encode_archive(&[
            ("manifest.json", manifest),
            ("source.bin", self.source_bytes.clone()),
        ])?;
        let archive_bytes = checked_len(bytes.len(), "trace artifact bytes")?;
        let maximum = trace_archive_limit(limits)?;
        if archive_bytes > maximum {
            return Err(DocsightError::ResourceLimit {
                resource: "trace artifact bytes".to_owned(),
                limit: maximum,
            });
        }
        Ok(bytes)
    }

    pub fn write(&self, path: &Path) -> Result<ArtifactWriteResult, DocsightError> {
        self.write_with_limits(path, ArtifactLimits::default())
    }

    pub fn write_with_limits(
        &self,
        path: &Path,
        limits: ArtifactLimits,
    ) -> Result<ArtifactWriteResult, DocsightError> {
        let bytes = self.to_bytes_with_limits(limits)?;
        write_all(path, &bytes)?;
        Ok(ArtifactWriteResult {
            sha256: sha256_hex(&bytes),
            bytes: checked_len(bytes.len(), "trace artifact bytes")?,
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DocsightError> {
        Self::from_bytes_with_limits(bytes, ArtifactLimits::default())
    }

    pub fn from_bytes_with_limits(
        bytes: &[u8],
        limits: ArtifactLimits,
    ) -> Result<Self, DocsightError> {
        let archive_bytes = checked_len(bytes.len(), "trace artifact bytes")?;
        let maximum = trace_archive_limit(limits)?;
        if archive_bytes > maximum {
            return Err(DocsightError::ResourceLimit {
                resource: "trace artifact bytes".to_owned(),
                limit: maximum,
            });
        }
        let entries = decode_archive(bytes, &["manifest.json", "source.bin"])?;
        let manifest: TraceManifest = deserialize_manifest(&entries[0], "trace")?;
        let artifact = Self {
            manifest,
            source_bytes: entries[1].clone(),
        };
        validate_trace_manifest(&artifact.manifest)?;
        if artifact.to_bytes_with_limits(limits)? != bytes {
            return Err(DocsightError::VerificationFailed {
                message: "trace archive is not canonical".to_owned(),
            });
        }
        Ok(artifact)
    }
}

impl ProofBundle {
    pub fn document_source(&self) -> Result<DocumentSource, DocsightError> {
        DocumentSource::from_bytes(self.source_bytes.clone())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, DocsightError> {
        self.to_bytes_with_limits(ArtifactLimits::default())
    }

    pub fn to_bytes_with_limits(&self, limits: ArtifactLimits) -> Result<Vec<u8>, DocsightError> {
        validate_proof_manifest(&self.manifest)?;
        let source_bytes = checked_len(self.source_bytes.len(), "proof source bytes")?;
        validate_document_size(source_bytes, limits, "proof source bytes")?;
        let manifest = canonical_json(&self.manifest)?;
        let manifest_bytes = checked_len(manifest.len(), "proof manifest bytes")?;
        if manifest_bytes > MAX_ARTIFACT_MANIFEST_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "proof manifest bytes".to_owned(),
                limit: MAX_ARTIFACT_MANIFEST_BYTES,
            });
        }
        let mut entries = vec![
            ("manifest.json", manifest),
            ("source.bin", self.source_bytes.clone()),
        ];
        if let Some(crop) = &self.crop_png {
            let crop_bytes = checked_len(crop.len(), "proof crop bytes")?;
            if crop_bytes > MAX_ARTIFACT_CROP_BYTES {
                return Err(DocsightError::ResourceLimit {
                    resource: "proof crop bytes".to_owned(),
                    limit: MAX_ARTIFACT_CROP_BYTES,
                });
            }
            entries.push(("crop.png", crop.clone()));
        }
        let bytes = encode_archive(&entries)?;
        let archive_bytes = checked_len(bytes.len(), "proof bundle bytes")?;
        let maximum = proof_archive_limit(limits)?;
        if archive_bytes > maximum {
            return Err(DocsightError::ResourceLimit {
                resource: "proof bundle bytes".to_owned(),
                limit: maximum,
            });
        }
        Ok(bytes)
    }

    pub fn write(&self, path: &Path) -> Result<ArtifactWriteResult, DocsightError> {
        self.write_with_limits(path, ArtifactLimits::default())
    }

    pub fn write_with_limits(
        &self,
        path: &Path,
        limits: ArtifactLimits,
    ) -> Result<ArtifactWriteResult, DocsightError> {
        let bytes = self.to_bytes_with_limits(limits)?;
        write_all(path, &bytes)?;
        Ok(ArtifactWriteResult {
            sha256: sha256_hex(&bytes),
            bytes: checked_len(bytes.len(), "proof bundle bytes")?,
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DocsightError> {
        Self::from_bytes_with_limits(bytes, ArtifactLimits::default())
    }

    pub fn from_bytes_with_limits(
        bytes: &[u8],
        limits: ArtifactLimits,
    ) -> Result<Self, DocsightError> {
        let archive_bytes = checked_len(bytes.len(), "proof bundle bytes")?;
        let maximum = proof_archive_limit(limits)?;
        if archive_bytes > maximum {
            return Err(DocsightError::ResourceLimit {
                resource: "proof bundle bytes".to_owned(),
                limit: maximum,
            });
        }
        let entries = decode_archive_flexible(bytes)?;
        if entries.len() != 2 && entries.len() != 3 {
            return Err(DocsightError::MalformedDocument {
                message:
                    "proof bundle must contain manifest.json, source.bin and optional crop.png"
                        .to_owned(),
            });
        }
        let expected = if entries.len() == 3 {
            ["manifest.json", "source.bin", "crop.png"].as_slice()
        } else {
            ["manifest.json", "source.bin"].as_slice()
        };
        for (entry, expected_name) in entries.iter().zip(expected) {
            if entry.0 != *expected_name {
                return Err(DocsightError::MalformedDocument {
                    message: "proof bundle entries are not canonical".to_owned(),
                });
            }
        }
        let manifest: ProofBundleManifest = deserialize_manifest(&entries[0].1, "proof bundle")?;
        let crop_png = if entries.len() == 3 {
            Some(entries[2].1.clone())
        } else {
            None
        };
        let bundle = Self {
            manifest,
            source_bytes: entries[1].1.clone(),
            crop_png,
        };
        validate_proof_manifest(&bundle.manifest)?;
        if bundle.manifest.crop.is_some() != bundle.crop_png.is_some() {
            return Err(DocsightError::MalformedDocument {
                message: "proof crop manifest and artifact entry disagree".to_owned(),
            });
        }
        if bundle.to_bytes_with_limits(limits)? != bytes {
            return Err(DocsightError::VerificationFailed {
                message: "proof bundle archive is not canonical".to_owned(),
            });
        }
        Ok(bundle)
    }
}

enum TraceMaterial {
    Docx {
        document: Document,
        pages: Vec<LaidOutPage>,
    },
    Pdf {
        document: Document,
        page: PdfTracePage,
    },
}

impl TraceMaterial {
    fn document(&self) -> &Document {
        match self {
            Self::Docx { document, .. } | Self::Pdf { document, .. } => document,
        }
    }

    fn page_bbox(&self, page_number: u32) -> Result<Rect, DocsightError> {
        let page =
            self.document()
                .page(page_number)
                .ok_or_else(|| DocsightError::ObjectNotFound {
                    object: format!("page {page_number}"),
                })?;
        Rect::new(0.0, 0.0, page.width_pt, page.height_pt)
    }

    fn trace_operations(
        &self,
        target_page: u32,
    ) -> (
        Vec<TraceGlyphRun>,
        Vec<TraceDisplayOperation>,
        TraceDecisionCoverage,
    ) {
        match self {
            Self::Docx { pages, .. } => {
                let mut glyph_runs = Vec::new();
                let mut operations = Vec::new();
                for page in pages.iter().filter(|page| page.number == target_page) {
                    for border in &page.borders {
                        operations.push(TraceDisplayOperation::Border {
                            bbox: border.rect,
                            color_argb: border.color_argb,
                        });
                    }
                    for run in &page.runs {
                        let text_sha256 = sha256_hex(run.text.as_bytes());
                        glyph_runs.push(TraceGlyphRun {
                            text_sha256: text_sha256.clone(),
                            bbox: run.bbox,
                            font_size_pt: run.font_size,
                            font_name: "docsight-proportional-reference".to_owned(),
                            bold: run.bold,
                            color_argb: run.color_argb,
                        });
                        operations.push(TraceDisplayOperation::Text {
                            text_sha256,
                            bbox: run.bbox,
                            font_size_pt: run.font_size,
                            font_name: "docsight-proportional-reference".to_owned(),
                            bold: run.bold,
                            color_argb: run.color_argb,
                            stroke_color_argb: run.color_argb,
                            stroke_width_pt: 1.0,
                            render_mode: 0,
                            mirrored_x: false,
                            clips: Vec::new(),
                        });
                    }
                }
                (
                    glyph_runs,
                    operations,
                    TraceDecisionCoverage {
                        resolved_resources: TraceDecisionStatus::Verified,
                        glyph_metrics: TraceDecisionStatus::Verified,
                        line_breaks: TraceDecisionStatus::Verified,
                        table_sizing: TraceDecisionStatus::Verified,
                        pagination: TraceDecisionStatus::Verified,
                        display_list_operations: TraceDecisionStatus::Verified,
                    },
                )
            }
            Self::Pdf { page, .. } => {
                let glyph_runs = page
                    .spans
                    .iter()
                    .map(|span| TraceGlyphRun {
                        text_sha256: sha256_hex(span.text.as_bytes()),
                        bbox: span.bbox,
                        font_size_pt: span.font_size_pt,
                        font_name: span.font_name.clone(),
                        bold: span.bold,
                        color_argb: span.argb,
                    })
                    .collect::<Vec<_>>();
                let operations = page.operations.iter().map(trace_pdf_operation).collect();
                (
                    glyph_runs,
                    operations,
                    TraceDecisionCoverage {
                        resolved_resources: TraceDecisionStatus::Verified,
                        glyph_metrics: TraceDecisionStatus::Verified,
                        line_breaks: TraceDecisionStatus::NotApplicable,
                        table_sizing: TraceDecisionStatus::NotApplicable,
                        pagination: TraceDecisionStatus::NotApplicable,
                        display_list_operations: TraceDecisionStatus::Verified,
                    },
                )
            }
        }
    }

    fn trace_table_sizing(&self, page: u32) -> Vec<TraceTableSizing> {
        match self {
            Self::Docx { document, .. } => trace_tables(document, page),
            Self::Pdf { .. } => Vec::new(),
        }
    }

    fn trace_pagination(&self, page: u32) -> Vec<TracePaginationDecision> {
        match self {
            Self::Docx { document, .. } => trace_pagination(document, page),
            Self::Pdf { .. } => Vec::new(),
        }
    }
}

fn trace_pdf_operation(operation: &PdfTraceDisplayOperation) -> TraceDisplayOperation {
    match operation {
        PdfTraceDisplayOperation::Text {
            text,
            bbox,
            font_size_pt,
            font_name,
            bold,
            color_argb,
            stroke_color_argb,
            stroke_width_pt,
            render_mode,
            mirrored_x,
            clips,
        } => TraceDisplayOperation::Text {
            text_sha256: sha256_hex(text.as_bytes()),
            bbox: *bbox,
            font_size_pt: *font_size_pt,
            font_name: font_name.clone(),
            bold: *bold,
            color_argb: *color_argb,
            stroke_color_argb: *stroke_color_argb,
            stroke_width_pt: *stroke_width_pt,
            render_mode: *render_mode,
            mirrored_x: *mirrored_x,
            clips: clips.clone(),
        },
        PdfTraceDisplayOperation::Fill {
            path,
            color_argb,
            fill_rule,
            clips,
        } => TraceDisplayOperation::Fill {
            path: path.clone(),
            color_argb: *color_argb,
            fill_rule: *fill_rule,
            clips: clips.clone(),
        },
        PdfTraceDisplayOperation::Stroke {
            path,
            color_argb,
            width_pt,
            line_cap,
            line_join,
            miter_limit,
            dash_pattern_pt,
            dash_phase_pt,
            clips,
        } => TraceDisplayOperation::Stroke {
            path: path.clone(),
            color_argb: *color_argb,
            width_pt: *width_pt,
            line_cap: *line_cap,
            line_join: *line_join,
            miter_limit: *miter_limit,
            dash_pattern_pt: dash_pattern_pt.clone(),
            dash_phase_pt: *dash_phase_pt,
            clips: clips.clone(),
        },
        PdfTraceDisplayOperation::Figure {
            bbox,
            resource_name,
            clips,
        } => TraceDisplayOperation::Figure {
            bbox: *bbox,
            resource_name: resource_name.clone(),
            clips: clips.clone(),
        },
    }
}

fn trace_material(
    source: &DocumentSource,
    page: u32,
    password: &[u8],
) -> Result<TraceMaterial, DocsightError> {
    match source.format() {
        DocumentFormat::Docx => {
            let laid_out = ingest_docx(source)?;
            Ok(TraceMaterial::Docx {
                document: laid_out.document,
                pages: laid_out.pages,
            })
        }
        DocumentFormat::Pdf => {
            let pdf = PdfDocument::open_with_password(source, password)?;
            let document = pdf.to_document()?;
            let page = pdf.trace_page(page)?;
            Ok(TraceMaterial::Pdf { document, page })
        }
    }
}

fn trace_resources(material: &TraceMaterial, source: &DocumentSource) -> Vec<TraceResource> {
    match material {
        TraceMaterial::Docx { document, .. } => document
            .resources
            .iter()
            .map(|resource| TraceResource {
                id: resource.id.to_string(),
                kind: resource.kind.into(),
                name: resource.name.clone(),
                target: resource.target.clone(),
                content_sha256: resource.content_sha256.clone(),
            })
            .collect(),
        TraceMaterial::Pdf { page, .. } => page
            .resources
            .iter()
            .map(|resource| {
                let kind = match resource.kind {
                    PdfTraceResourceKind::Font => TraceResourceKind::Font,
                    PdfTraceResourceKind::GraphicsState => TraceResourceKind::GraphicsState,
                    PdfTraceResourceKind::XObject => TraceResourceKind::XObject,
                };
                let path = format!(
                    "pdf::resource::{}::{}",
                    trace_resource_kind_name(kind),
                    resource.name
                );
                TraceResource {
                    id: ObjectId::new("res", source.sha256(), &path).to_string(),
                    kind,
                    name: resource.name.clone(),
                    target: resource.target.clone(),
                    content_sha256: resource.content_sha256.clone(),
                }
            })
            .collect(),
    }
}

fn trace_resource_kind_name(kind: TraceResourceKind) -> &'static str {
    match kind {
        TraceResourceKind::Font => "font",
        TraceResourceKind::Image => "image",
        TraceResourceKind::Relationship => "relationship",
        TraceResourceKind::EmbeddedObject => "embedded_object",
        TraceResourceKind::GraphicsState => "graphics_state",
        TraceResourceKind::XObject => "x_object",
    }
}

impl From<ResourceKind> for TraceResourceKind {
    fn from(value: ResourceKind) -> Self {
        match value {
            ResourceKind::Font => Self::Font,
            ResourceKind::Image => Self::Image,
            ResourceKind::Relationship => Self::Relationship,
            ResourceKind::EmbeddedObject => Self::EmbeddedObject,
        }
    }
}

fn trace_tables(document: &Document, page: u32) -> Vec<TraceTableSizing> {
    document
        .tables()
        .filter(|(block, _)| block.page == Some(page))
        .map(|(block, table)| TraceTableSizing {
            object_id: block.id.to_string(),
            page,
            bbox: block.bbox,
            rows: table.rows,
            columns: table.columns,
            column_widths_pt: table.column_widths_pt.clone(),
            detector: table.detector.clone(),
        })
        .collect()
}

fn trace_pagination(document: &Document, page: u32) -> Vec<TracePaginationDecision> {
    document
        .blocks_on_page(page)
        .map(|block| TracePaginationDecision {
            object_id: block.id.to_string(),
            kind: block.kind,
            page,
            bbox: block.bbox_on_page(page),
            reading_order: block.reading_order,
        })
        .collect()
}

fn selected_evidence(
    document: &Document,
    source: &DocumentSource,
    target: &TraceTarget,
    render_fingerprint: String,
    glyph_coverage: f32,
) -> Result<Vec<EvidenceRecord>, DocsightError> {
    let object_ids = match &target.selector {
        TraceSelector::Object { id } => vec![ObjectId::from_raw(id)],
        TraceSelector::Page { page } => document
            .blocks_on_page(*page)
            .map(|block| block.id.clone())
            .collect(),
        TraceSelector::Region { page, bbox } => document
            .blocks_on_page(*page)
            .filter(|block| {
                block
                    .bbox_on_page(*page)
                    .is_some_and(|block_bbox| block_bbox.intersects(*bbox))
            })
            .map(|block| block.id.clone())
            .collect(),
    };
    object_ids
        .iter()
        .map(|id| {
            compute_evidence(
                document,
                source,
                id,
                Some(render_fingerprint.clone()),
                glyph_coverage,
            )
        })
        .collect()
}

pub fn reproduction_fingerprint(source: &DocumentSource) -> ReproductionFingerprint {
    let engine = format!("docsight {}", env!("CARGO_PKG_VERSION"));
    let ooxml_engine = format!("docsight-ooxml {}", env!("CARGO_PKG_VERSION"));
    let pdf_engine = format!("docsight-pdf {}", env!("CARGO_PKG_VERSION"));
    let raster_engine = format!("docsight-render {}", env!("CARGO_PKG_VERSION"));
    let fonts = format!(
        "layout:{}|raster:{}",
        docsight_layout::font_fingerprint(),
        crate::raster_font_fingerprint()
    );
    let layout_profile = "agent-fidelity-v1".to_owned();
    let values = [
        source.sha256(),
        &engine,
        &ooxml_engine,
        &pdf_engine,
        &raster_engine,
        &fonts,
        &layout_profile,
    ];
    let result_fingerprint = sha256_hex(&values.join("|").into_bytes());
    ReproductionFingerprint {
        engine,
        ooxml_engine,
        pdf_engine,
        raster_engine,
        fonts,
        layout_profile,
        result_fingerprint,
    }
}

fn validate_trace_manifest(manifest: &TraceManifest) -> Result<(), DocsightError> {
    if manifest.schema != TRACE_SCHEMA {
        return Err(DocsightError::MalformedDocument {
            message: "trace schema is not supported".to_owned(),
        });
    }
    if manifest.source.document_sha256.len() != 64
        || !manifest
            .source
            .document_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(DocsightError::MalformedDocument {
            message: "trace document SHA-256 is invalid".to_owned(),
        });
    }
    if manifest.raster.media_type != "image/png" {
        return Err(DocsightError::MalformedDocument {
            message: "trace raster media type is not image/png".to_owned(),
        });
    }
    if manifest.decision_coverage.table_sizing == TraceDecisionStatus::NotApplicable
        && !manifest.table_sizing.is_empty()
    {
        return Err(DocsightError::MalformedDocument {
            message: "trace contains table-sizing decisions marked not applicable".to_owned(),
        });
    }
    if manifest.decision_coverage.pagination == TraceDecisionStatus::NotApplicable
        && !manifest.pagination.is_empty()
    {
        return Err(DocsightError::MalformedDocument {
            message: "trace contains pagination decisions marked not applicable".to_owned(),
        });
    }
    let mut display_list = manifest.display_list.clone();
    let expected_display_hash = display_list.sha256.clone();
    display_list.sha256.clear();
    if expected_display_hash != canonical_sha256(&display_list)? {
        return Err(DocsightError::VerificationFailed {
            message: "trace display list fingerprint is invalid".to_owned(),
        });
    }
    Ok(())
}

fn validate_proof_manifest(manifest: &ProofBundleManifest) -> Result<(), DocsightError> {
    if manifest.schema != PROOF_BUNDLE_SCHEMA {
        return Err(DocsightError::MalformedDocument {
            message: "proof bundle schema is not supported".to_owned(),
        });
    }
    validate_trace_manifest(&manifest.trace)?;
    let selected = selected_ids_from_manifest(manifest)?;
    let evidence_ids = manifest
        .evidence
        .iter()
        .map(|evidence| evidence.object_id.to_string())
        .collect::<Vec<_>>();
    if selected != evidence_ids {
        return Err(DocsightError::MalformedDocument {
            message: "proof bundle evidence does not match its selected target".to_owned(),
        });
    }
    if let Some(crop) = &manifest.crop
        && (crop.media_type != "image/png" || crop.sha256.len() != 64)
    {
        return Err(DocsightError::MalformedDocument {
            message: "proof crop metadata is invalid".to_owned(),
        });
    }
    Ok(())
}

fn selected_ids_from_manifest(
    manifest: &ProofBundleManifest,
) -> Result<Vec<String>, DocsightError> {
    match &manifest.trace.target.selector {
        TraceSelector::Object { id } => Ok(vec![id.clone()]),
        TraceSelector::Page { .. } | TraceSelector::Region { .. } => Ok(manifest
            .evidence
            .iter()
            .map(|evidence| evidence.object_id.to_string())
            .collect()),
    }
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, DocsightError> {
    serde_json::to_vec(value).map_err(|error| DocsightError::BackendFailure {
        backend: "artifact serializer".to_owned(),
        message: error.to_string(),
    })
}

fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, DocsightError> {
    Ok(sha256_hex(&canonical_json(value)?))
}

fn deserialize_manifest<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    artifact: &str,
) -> Result<T, DocsightError> {
    if bytes.len() as u64 > MAX_ARTIFACT_MANIFEST_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: format!("{artifact} manifest bytes"),
            limit: MAX_ARTIFACT_MANIFEST_BYTES,
        });
    }
    serde_json::from_slice(bytes).map_err(|error| DocsightError::MalformedDocument {
        message: format!("{artifact} manifest is invalid: {error}"),
    })
}

fn encode_archive(entries: &[(&str, Vec<u8>)]) -> Result<Vec<u8>, DocsightError> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    for (name, bytes) in entries {
        writer
            .start_file(*name, options)
            .map_err(artifact_write_error)?;
        std::io::Write::write_all(&mut writer, bytes).map_err(artifact_write_error)?;
    }
    let cursor = writer.finish().map_err(artifact_write_error)?;
    Ok(cursor.into_inner())
}

fn decode_archive(bytes: &[u8], expected: &[&str]) -> Result<Vec<Vec<u8>>, DocsightError> {
    let entries = decode_archive_flexible(bytes)?;
    if entries.len() != expected.len() {
        return Err(DocsightError::MalformedDocument {
            message: "artifact entry count is invalid".to_owned(),
        });
    }
    let mut data = Vec::with_capacity(entries.len());
    for ((name, entry), expected_name) in entries.into_iter().zip(expected) {
        if name != *expected_name {
            return Err(DocsightError::MalformedDocument {
                message: "artifact entries are not canonical".to_owned(),
            });
        }
        data.push(entry);
    }
    Ok(data)
}

fn decode_archive_flexible(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, DocsightError> {
    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|error| DocsightError::MalformedDocument {
            message: format!("artifact ZIP is invalid: {error}"),
        })?;
    let entry_count = archive.len();
    if entry_count > 3 {
        return Err(DocsightError::ResourceLimit {
            resource: "artifact entries".to_owned(),
            limit: 3,
        });
    }
    let mut entries = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let mut file =
            archive
                .by_index(index)
                .map_err(|error| DocsightError::MalformedDocument {
                    message: format!("artifact entry is invalid: {error}"),
                })?;
        let name = file.name().to_owned();
        let limit = match name.as_str() {
            "manifest.json" => MAX_ARTIFACT_MANIFEST_BYTES,
            "source.bin" => docsight_core::MAX_INSPECT_BYTES,
            "crop.png" => MAX_ARTIFACT_CROP_BYTES,
            _ => {
                return Err(DocsightError::MalformedDocument {
                    message: "artifact contains an unsupported entry".to_owned(),
                });
            }
        };
        if file.compression() != CompressionMethod::Stored || file.size() > limit {
            return Err(DocsightError::ResourceLimit {
                resource: format!("artifact entry {name}"),
                limit,
            });
        }
        let mut entry = Vec::new();
        file.read_to_end(&mut entry).map_err(artifact_read_error)?;
        if entry.len() as u64 != file.size() {
            return Err(DocsightError::MalformedDocument {
                message: "artifact entry size is inconsistent".to_owned(),
            });
        }
        entries.push((name, entry));
    }
    Ok(entries)
}

fn read_artifact(path: &Path, limit: u64, resource: &str) -> Result<Vec<u8>, DocsightError> {
    let file = File::open(path).map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let read_limit = limit
        .checked_add(1)
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: resource.to_owned(),
            limit,
        })?;
    let mut reader = file.take(read_limit);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if checked_len(bytes.len(), resource)? > limit {
        return Err(DocsightError::ResourceLimit {
            resource: resource.to_owned(),
            limit,
        });
    }
    Ok(bytes)
}

fn artifact_write_error(error: impl std::fmt::Display) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "artifact writer".to_owned(),
        message: error.to_string(),
    }
}

fn artifact_read_error(error: std::io::Error) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("artifact entry cannot be read: {error}"),
    }
}

fn checked_len(length: usize, resource: &str) -> Result<u64, DocsightError> {
    u64::try_from(length).map_err(|_| DocsightError::ResourceLimit {
        resource: resource.to_owned(),
        limit: u64::MAX,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
