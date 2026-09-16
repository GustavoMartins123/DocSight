use crate::{
    Block, BlockKind, DocsightError, Document, DocumentFormat, DocumentSource, ObjectId, Rect,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const TEXT_LOSS_CODES: &[&str] = &["DOCX_RUN_ELEMENT_UNSUPPORTED"];

const GLOBAL_TEXT_LOSS_CODES: &[&str] = &["PDF_TEXT_CODE_UNMAPPED"];

const RESOURCE_LOSS_CODES: &[&str] = &[
    "DOCX_FIGURE_RASTER_PLACEHOLDER",
    "DOCX_IMAGE_UNRESOLVED",
    "DOCX_EMBEDDED_OBJECT_INERT",
    "DOCX_ACTIVE_CONTENT_INERT",
    "PDF_XOBJECT_PLACEHOLDER",
];

const GEOMETRY_WARNING_CODES: &[&str] = &[
    "DOCX_TABLE_GRID_WIDTHS_UNUSABLE",
    "DOCX_BLOCK_TALLER_THAN_PAGE",
];

const GLOBAL_GEOMETRY_APPROXIMATION_CODES: &[&str] = &[
    "DOCX_FONT_SUBSTITUTED",
    "DOCX_PAGINATION_BLOCK_GRANULAR",
    "APPROXIMATED_PDF_FONT",
];

const GLOBAL_VISUAL_UNSUPPORTED_CODES: &[&str] = &[
    "DOCX_FONT_SUBSTITUTED",
    "APPROXIMATED_PDF_FONT",
    "PDF_XOBJECT_PLACEHOLDER",
    "PDF_EXTGSTATE_IGNORED",
    "PDF_CLIP_TEXT_VISUAL",
    "PDF_NEGATIVE_FONT_SIZE_VISUAL",
    "PDF_SOFT_MASK_IGNORED",
    "PDF_COLOR_SPACE_UNSUPPORTED",
    "PDF_PATTERN_PAINT_UNSUPPORTED",
    "PDF_BLEND_MODE_UNSUPPORTED",
    "PDF_NON_UNIFORM_STROKE_VISUAL",
    "PDF_SHADING_UNSUPPORTED",
];

const PAGE_APPROXIMATION_CODES: &[&str] = &["DOCX_LAYOUT_PAGINATED"];

const DOCX_RENDER_APPROXIMATION_CODES: &[&str] = &[
    "DOCX_FONT_SUBSTITUTED",
    "DOCX_FIGURE_RASTER_PLACEHOLDER",
    "DOCX_BLOCK_TALLER_THAN_PAGE",
    "DOCX_PAGINATION_BLOCK_GRANULAR",
];

const DOCX_TEXT_APPROXIMATION_CODE: &str = "DOCX_RUN_ELEMENT_UNSUPPORTED";

const UNKNOWN_STRUCTURE_PENALTY: f32 = 1.000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    #[default]
    Exact,
    EvidenceLimited,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pagination {
    #[default]
    Native,
    Computed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageFidelity {
    pub pagination: Pagination,
    pub authoritative: bool,
    pub evidence_status: EvidenceStatus,
    pub reason_codes: Vec<String>,
}

pub fn page_fidelity(document: &Document) -> PageFidelity {
    let codes: BTreeSet<&str> = document
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    let pagination = if codes
        .iter()
        .any(|code| PAGE_APPROXIMATION_CODES.contains(code))
    {
        Pagination::Computed
    } else {
        Pagination::Native
    };
    let reason_codes: Vec<String> = codes
        .into_iter()
        .filter(|code| {
            PAGE_APPROXIMATION_CODES.contains(code)
                || GLOBAL_GEOMETRY_APPROXIMATION_CODES.contains(code)
                || GEOMETRY_WARNING_CODES.contains(code)
        })
        .map(str::to_owned)
        .collect();
    let authoritative = reason_codes.is_empty();
    PageFidelity {
        pagination,
        authoritative,
        evidence_status: if authoritative {
            EvidenceStatus::Exact
        } else {
            EvidenceStatus::EvidenceLimited
        },
        reason_codes,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FidelityProfile {
    pub text: f32,
    pub structure: f32,
    pub geometry: f32,
    pub visual: f32,
    pub reasons: Vec<String>,
}

impl FidelityProfile {
    pub fn overall(&self) -> f32 {
        (self.text + self.structure + self.geometry + self.visual) / 4.0
    }
}

pub struct FidelityInputs<'a> {
    pub blocks: &'a [&'a Block],
    pub glyph_coverage: f32,
    pub single_block_confidence: Option<f32>,
}

fn measured_fidelity(doc: &Document, inputs: &FidelityInputs<'_>) -> FidelityProfile {
    let total = inputs.blocks.len();
    let block_ids: BTreeSet<&ObjectId> = inputs.blocks.iter().map(|block| &block.id).collect();
    let relevant_warnings: Vec<_> = doc
        .warnings
        .iter()
        .filter(|warning| {
            warning
                .object
                .as_ref()
                .is_none_or(|object| block_ids.contains(object))
        })
        .collect();
    let reasons: BTreeSet<String> = relevant_warnings
        .iter()
        .map(|warning| warning.code.clone())
        .collect();
    let text_loss_objects: BTreeSet<&ObjectId> = doc
        .warnings
        .iter()
        .filter(|warning| TEXT_LOSS_CODES.contains(&warning.code.as_str()))
        .filter_map(|warning| warning.object.as_ref())
        .collect();
    let text_blocks_total = inputs
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.kind,
                BlockKind::Paragraph | BlockKind::Heading | BlockKind::ListItem | BlockKind::Note
            )
        })
        .count();
    let text_blocks_affected = inputs
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.kind,
                BlockKind::Paragraph | BlockKind::Heading | BlockKind::ListItem | BlockKind::Note
            ) && text_loss_objects.contains(&block.id)
        })
        .count();
    let text = if GLOBAL_TEXT_LOSS_CODES
        .iter()
        .any(|code| reasons.contains(*code))
    {
        0.0
    } else if text_blocks_total == 0 {
        1.0
    } else {
        1.0 - text_blocks_affected as f32 / text_blocks_total as f32
    };

    let structure = match doc.format {
        DocumentFormat::Docx => {
            if total == 0 {
                1.0
            } else {
                let unknown = inputs
                    .blocks
                    .iter()
                    .filter(|block| block.kind == BlockKind::Unknown)
                    .count();
                1.0 - UNKNOWN_STRUCTURE_PENALTY * unknown as f32 / total as f32
            }
        }
        DocumentFormat::Pdf => match inputs.single_block_confidence {
            Some(confidence) => confidence,
            None => {
                if total == 0 {
                    1.0
                } else {
                    let sum: f32 = inputs.blocks.iter().map(|block| block.confidence).sum();
                    sum / total as f32
                }
            }
        },
    };

    let placed = inputs
        .blocks
        .iter()
        .filter(|block| block.page.is_some() && block.bbox.is_some())
        .count();
    let placed_ratio = if total == 0 {
        1.0
    } else {
        placed as f32 / total as f32
    };
    let geometry_warning_objects: BTreeSet<&ObjectId> = doc
        .warnings
        .iter()
        .filter(|warning| GEOMETRY_WARNING_CODES.contains(&warning.code.as_str()))
        .filter_map(|warning| warning.object.as_ref())
        .collect();
    let geometry_affected = inputs
        .blocks
        .iter()
        .filter(|block| geometry_warning_objects.contains(&block.id))
        .count();
    let geometry_exact_ratio = if total == 0 {
        1.0
    } else {
        1.0 - geometry_affected as f32 / total as f32
    };
    let geometry = if GLOBAL_GEOMETRY_APPROXIMATION_CODES
        .iter()
        .any(|code| reasons.contains(*code))
    {
        0.0
    } else {
        placed_ratio * geometry_exact_ratio
    };

    let figures_total = inputs
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Figure)
        .count();
    let placeholder_figures = inputs
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Figure)
        .filter(|block| {
            doc.warnings.iter().any(|warning| {
                warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER"
                    && warning.object.as_ref() == Some(&block.id)
            })
        })
        .count();
    let placeholder_share = if figures_total == 0 {
        0.0
    } else {
        placeholder_figures as f32 / figures_total as f32
    };
    let visual = if GLOBAL_VISUAL_UNSUPPORTED_CODES
        .iter()
        .any(|code| reasons.contains(*code))
    {
        0.0
    } else {
        inputs.glyph_coverage * (1.0 - placeholder_share)
    };

    FidelityProfile {
        text,
        structure,
        geometry,
        visual,
        reasons: reasons.into_iter().collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub document_digest: String,
    pub object_id: ObjectId,
    pub kind: BlockKind,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    pub z_index: i32,
    pub reading_order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub fidelity: FidelityProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_fingerprint: Option<String>,
    pub text_fragment: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Exact,
    Inferred,
    Approximated,
    Unsupported,
}

impl CoverageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Approximated => "approximated",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverageMetric {
    pub score: f32,
    pub status: CoverageStatus,
    pub reason_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverageRegion {
    pub page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    pub status: CoverageStatus,
    pub reason_code: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageCoverage {
    pub page: u32,
    pub text: CoverageMetric,
    pub structure: CoverageMetric,
    pub geometry: CoverageMetric,
    pub visual: CoverageMetric,
    pub resource: CoverageMetric,
    pub overall_fidelity: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<CoverageRegion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub document_sha256: String,
    pub format: DocumentFormat,
    pub global: PageCoverage,
    pub pages: Vec<PageCoverage>,
    pub affected_objects_count: usize,
    pub unsupported_feature_count: usize,
    pub reason_codes: Vec<String>,
}

pub fn compute_evidence(
    doc: &Document,
    source: &DocumentSource,
    object_id: &ObjectId,
    render_fingerprint: Option<String>,
    glyph_coverage: f32,
) -> Result<EvidenceRecord, DocsightError> {
    let block =
        doc.find_block(object_id.as_str())
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: object_id.to_string(),
            })?;

    let fidelity = measured_fidelity(
        doc,
        &FidelityInputs {
            blocks: &[block],
            glyph_coverage,
            single_block_confidence: Some(block.confidence),
        },
    );

    let full_text = block.text();
    let text_fragment = truncate_at_char_boundary(&full_text, 300);

    Ok(EvidenceRecord {
        document_digest: source.sha256().to_owned(),
        object_id: block.id.clone(),
        kind: block.kind,
        source_path: block.source.path.clone(),
        source_offset: block.source.offset,
        page: block.page,
        bbox: block.bbox,
        z_index: block.z_index,
        reading_order: block.reading_order,
        confidence: if block.confidence < 0.999 {
            Some(block.confidence)
        } else {
            None
        },
        fidelity,
        render_fingerprint,
        text_fragment,
    })
}

fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}...", &text[..boundary])
}

fn global_region_kind(code: &str) -> (CoverageStatus, &'static str) {
    if GLOBAL_TEXT_LOSS_CODES.contains(&code) {
        (
            CoverageStatus::Approximated,
            "the page shows text codes the source encoding does not map to characters",
        )
    } else if GLOBAL_VISUAL_UNSUPPORTED_CODES.contains(&code) {
        (
            CoverageStatus::Unsupported,
            "the page is not backed by source-faithful geometry or rendering",
        )
    } else {
        (
            CoverageStatus::Approximated,
            "the page is not backed by source-faithful geometry or rendering",
        )
    }
}

fn metric_reason_codes(profile: &FidelityProfile, relevant: &[&str]) -> Vec<String> {
    profile
        .reasons
        .iter()
        .filter(|reason| relevant.contains(&reason.as_str()))
        .cloned()
        .collect()
}

fn coverage_metric(
    score: f32,
    exact_status: CoverageStatus,
    degraded_status: CoverageStatus,
    reasons: Vec<String>,
    unsupported: bool,
) -> CoverageMetric {
    let status = if unsupported {
        CoverageStatus::Unsupported
    } else if score >= 0.999 {
        exact_status
    } else {
        degraded_status
    };
    CoverageMetric {
        score,
        status,
        reason_codes: reasons,
    }
}

struct CoverageAccumulator<'a> {
    affected_ids: &'a mut BTreeSet<String>,
    reason_codes: &'a mut BTreeSet<String>,
}

fn page_coverage(
    doc: &Document,
    page: u32,
    page_blocks: &[&Block],
    glyph_coverage: f32,
    include_regions: bool,
    is_global: bool,
    accumulator: &mut CoverageAccumulator<'_>,
) -> PageCoverage {
    let affected_ids = &mut *accumulator.affected_ids;
    let all_reason_codes = &mut *accumulator.reason_codes;
    let fidelity = measured_fidelity(
        doc,
        &FidelityInputs {
            blocks: page_blocks,
            glyph_coverage,
            single_block_confidence: None,
        },
    );
    for reason in &fidelity.reasons {
        all_reason_codes.insert(reason.clone());
    }

    let mut regions = Vec::new();

    for code in GLOBAL_GEOMETRY_APPROXIMATION_CODES
        .iter()
        .chain(GLOBAL_VISUAL_UNSUPPORTED_CODES.iter())
        .chain(GLOBAL_TEXT_LOSS_CODES.iter())
    {
        if fidelity.reasons.iter().any(|reason| reason == code) {
            for block in page_blocks {
                affected_ids.insert(block.id.to_string());
            }
            if include_regions
                && !regions
                    .iter()
                    .any(|region: &CoverageRegion| region.reason_code == *code)
            {
                let (status, description) = global_region_kind(code);
                regions.push(CoverageRegion {
                    page,
                    object_id: None,
                    bbox: None,
                    status,
                    reason_code: (*code).to_owned(),
                    description: description.to_owned(),
                });
            }
        }
    }

    for block in page_blocks {
        if block.kind == BlockKind::Unknown {
            affected_ids.insert(block.id.to_string());
            all_reason_codes.insert("DOCX_BODY_ELEMENT_UNSUPPORTED".to_owned());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: block.bbox,
                    status: CoverageStatus::Unsupported,
                    reason_code: "DOCX_BODY_ELEMENT_UNSUPPORTED".to_owned(),
                    description:
                        "element preserved as an opaque node without semantic interpretation"
                            .to_owned(),
                });
            }
        }
        if block.kind == BlockKind::Figure
            && doc.warnings.iter().any(|warning| {
                warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER"
                    && warning.object.as_ref() == Some(&block.id)
            })
        {
            affected_ids.insert(block.id.to_string());
            all_reason_codes.insert("DOCX_FIGURE_RASTER_PLACEHOLDER".to_owned());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: block.bbox,
                    status: CoverageStatus::Unsupported,
                    reason_code: "DOCX_FIGURE_RASTER_PLACEHOLDER".to_owned(),
                    description: "embedded image bytes are not rasterized; visual evidence is a placeholder box"
                        .to_owned(),
                });
            }
        }
        if doc.warnings.iter().any(|warning| {
            TEXT_LOSS_CODES.contains(&warning.code.as_str())
                && warning.object.as_ref() == Some(&block.id)
        }) {
            affected_ids.insert(block.id.to_string());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: block.bbox,
                    status: CoverageStatus::Approximated,
                    reason_code: "DOCX_RUN_ELEMENT_UNSUPPORTED".to_owned(),
                    description:
                        "paragraph text may be incomplete because run content was not extracted"
                            .to_owned(),
                });
            }
        }
        for warning in doc.warnings.iter().filter(|warning| {
            GEOMETRY_WARNING_CODES.contains(&warning.code.as_str())
                && warning.object.as_ref() == Some(&block.id)
        }) {
            affected_ids.insert(block.id.to_string());
            all_reason_codes.insert(warning.code.clone());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: block.bbox,
                    status: CoverageStatus::Approximated,
                    reason_code: warning.code.clone(),
                    description: warning.effect.clone(),
                });
            }
        }
        if doc.format == DocumentFormat::Pdf && block.confidence < 0.95 {
            affected_ids.insert(block.id.to_string());
            let reason = if block.kind == BlockKind::Table {
                "INFERRED_TABLE"
            } else {
                "INFERRED_SEMANTICS"
            };
            all_reason_codes.insert(reason.to_owned());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: block.bbox,
                    status: CoverageStatus::Inferred,
                    reason_code: reason.to_owned(),
                    description: format!(
                        "semantic structure inferred with confidence {:.3}",
                        block.confidence
                    ),
                });
            }
        }
    }

    let text_reasons = metric_reason_codes(&fidelity, TEXT_LOSS_CODES);
    let structure_unsupported = !page_blocks.is_empty()
        && page_blocks
            .iter()
            .all(|block| block.kind == BlockKind::Unknown);
    let geometry_reasons = metric_reason_codes(
        &fidelity,
        &[
            "DOCX_FONT_SUBSTITUTED",
            "DOCX_PAGINATION_BLOCK_GRANULAR",
            "DOCX_TABLE_GRID_WIDTHS_UNUSABLE",
            "DOCX_BLOCK_TALLER_THAN_PAGE",
        ],
    );
    let visual_reasons = metric_reason_codes(
        &fidelity,
        &[
            "DOCX_FONT_SUBSTITUTED",
            "APPROXIMATED_PDF_FONT",
            "PDF_XOBJECT_PLACEHOLDER",
            "DOCX_FIGURE_RASTER_PLACEHOLDER",
        ],
    );
    let geometry_unsupported = GLOBAL_GEOMETRY_APPROXIMATION_CODES
        .iter()
        .any(|code| fidelity.reasons.iter().any(|reason| reason == code));
    let visual_unsupported = GLOBAL_VISUAL_UNSUPPORTED_CODES
        .iter()
        .any(|code| fidelity.reasons.iter().any(|reason| reason == code));

    PageCoverage {
        page,
        text: coverage_metric(
            fidelity.text,
            CoverageStatus::Exact,
            CoverageStatus::Approximated,
            text_reasons,
            false,
        ),
        structure: coverage_metric(
            fidelity.structure,
            CoverageStatus::Exact,
            CoverageStatus::Inferred,
            Vec::new(),
            structure_unsupported,
        ),
        geometry: coverage_metric(
            fidelity.geometry,
            CoverageStatus::Exact,
            CoverageStatus::Approximated,
            geometry_reasons,
            geometry_unsupported,
        ),
        visual: coverage_metric(
            fidelity.visual,
            CoverageStatus::Exact,
            CoverageStatus::Approximated,
            visual_reasons,
            visual_unsupported,
        ),
        resource: resource_coverage(doc, page_blocks, is_global),
        overall_fidelity: fidelity.overall(),
        regions,
    }
}

fn resource_loss_objects(doc: &Document) -> BTreeSet<&ObjectId> {
    doc.warnings
        .iter()
        .filter(|warning| RESOURCE_LOSS_CODES.contains(&warning.code.as_str()))
        .filter_map(|warning| warning.object.as_ref())
        .collect()
}

fn resource_coverage(doc: &Document, page_blocks: &[&Block], is_global: bool) -> CoverageMetric {
    let lost_objects = resource_loss_objects(doc);
    let mut total = 0usize;
    let mut lost = 0usize;

    for block in page_blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Figure)
    {
        total += 1;
        if lost_objects.contains(&block.id) {
            lost += 1;
        }
    }

    if is_global {
        for resource in &doc.resources {
            total += 1;
            if lost_objects.contains(&resource.id) || resource.content_sha256.is_none() {
                lost += 1;
            }
        }
    }

    let reasons: Vec<String> = doc
        .warnings
        .iter()
        .filter(|warning| RESOURCE_LOSS_CODES.contains(&warning.code.as_str()))
        .map(|warning| warning.code.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();

    if total == 0 {
        return CoverageMetric {
            score: 1.0,
            status: CoverageStatus::Exact,
            reason_codes: Vec::new(),
        };
    }
    let score = 1.0 - lost as f32 / total as f32;
    coverage_metric(
        score,
        CoverageStatus::Exact,
        CoverageStatus::Approximated,
        reasons,
        lost == total,
    )
}

fn unsupported_feature_count(doc: &Document) -> usize {
    doc.warnings
        .iter()
        .filter(|warning| {
            let code = warning.code.as_str();
            RESOURCE_LOSS_CODES.contains(&code)
                || GLOBAL_VISUAL_UNSUPPORTED_CODES.contains(&code)
                || GLOBAL_GEOMETRY_APPROXIMATION_CODES.contains(&code)
                || GLOBAL_TEXT_LOSS_CODES.contains(&code)
                || TEXT_LOSS_CODES.contains(&code)
                || GEOMETRY_WARNING_CODES.contains(&code)
                || code.contains("UNSUPPORTED")
                || code.contains("INERT")
                || code.contains("UNRESOLVED")
        })
        .count()
}

pub fn compute_coverage(
    doc: &Document,
    source: &DocumentSource,
    page_filter: Option<u32>,
    include_regions: bool,
    glyph_coverage: f32,
) -> Result<CoverageReport, DocsightError> {
    let mut affected_ids = BTreeSet::new();
    let mut all_reason_codes = BTreeSet::new();

    let pages_to_process: Vec<u32> = match page_filter {
        Some(page) => vec![page],
        None => doc.pages.iter().map(|page| page.number).collect(),
    };

    let mut page_coverages = Vec::with_capacity(pages_to_process.len());
    for page in pages_to_process {
        let page_blocks: Vec<&Block> = doc
            .blocks
            .iter()
            .filter(|block| block.page == Some(page))
            .collect();
        page_coverages.push(page_coverage(
            doc,
            page,
            &page_blocks,
            glyph_coverage,
            include_regions,
            false,
            &mut CoverageAccumulator {
                affected_ids: &mut affected_ids,
                reason_codes: &mut all_reason_codes,
            },
        ));
    }

    let all_blocks: Vec<&Block> = doc.blocks.iter().collect();
    let global = page_coverage(
        doc,
        0,
        &all_blocks,
        glyph_coverage,
        false,
        true,
        &mut CoverageAccumulator {
            affected_ids: &mut affected_ids,
            reason_codes: &mut all_reason_codes,
        },
    );

    let reason_codes: Vec<String> = all_reason_codes.into_iter().collect();

    Ok(CoverageReport {
        document_sha256: source.sha256().to_owned(),
        format: source.format(),
        global,
        pages: page_coverages,
        affected_objects_count: affected_ids.len(),
        unsupported_feature_count: unsupported_feature_count(doc),
        reason_codes,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentCapabilities {
    pub structure: CoverageStatus,
    pub text: CoverageStatus,
    pub render: CoverageStatus,
}

pub fn document_capabilities(document: &Document) -> DocumentCapabilities {
    let codes: BTreeSet<&str> = document
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    match document.format {
        DocumentFormat::Docx => {
            DocumentCapabilities {
                structure: approximated_unless(codes.iter().any(|code| {
                    code.contains("UNSUPPORTED") && *code != DOCX_TEXT_APPROXIMATION_CODE
                })),
                text: approximated_unless(codes.contains(DOCX_TEXT_APPROXIMATION_CODE)),
                render: approximated_unless(
                    codes
                        .iter()
                        .any(|code| DOCX_RENDER_APPROXIMATION_CODES.contains(code)),
                ),
            }
        }
        DocumentFormat::Pdf => DocumentCapabilities {
            structure: CoverageStatus::Inferred,
            text: approximated_unless(
                codes
                    .iter()
                    .any(|code| GLOBAL_TEXT_LOSS_CODES.contains(code)),
            ),
            render: approximated_unless(
                codes
                    .iter()
                    .any(|code| GLOBAL_VISUAL_UNSUPPORTED_CODES.contains(code)),
            ),
        },
    }
}

fn approximated_unless(approximated: bool) -> CoverageStatus {
    if approximated {
        CoverageStatus::Approximated
    } else {
        CoverageStatus::Exact
    }
}
