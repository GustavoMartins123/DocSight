use crate::{
    Block, BlockKind, Diagnostic, DocsightError, Document, DocumentFormat, DocumentObject,
    DocumentSource, ObjectId, OverlayKind, Rect,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    "DOCX_WIDOW_CONTROL_RELAXED",
    "DOCX_SHAPE_VISUAL_OMITTED",
];

const GLOBAL_GEOMETRY_APPROXIMATION_CODES: &[&str] = &[
    "DOCX_FONT_SUBSTITUTED",
    "DOCX_PAGINATION_BLOCK_GRANULAR",
    "DOCX_SECTION_COLUMNS_UNSUPPORTED",
    "DOCX_SECTION_DEFAULTED",
    "DOCX_SECTION_GUTTER_IGNORED",
    "DOCX_SECTION_GEOMETRY_DEFAULTED",
    "DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED",
    "DOCX_HEADER_FOOTER_OVERLAPS_BODY",
    "DOCX_HEADER_FOOTER_OUTSIDE_PAGE",
    "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED",
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
    "DOCX_HEADER_FOOTER_UNRESOLVED",
    "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED",
    "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED",
    "DOCX_SHAPE_VISUAL_OMITTED",
];

const PAGE_APPROXIMATION_CODES: &[&str] = &["DOCX_LAYOUT_PAGINATED"];

const DOCX_RENDER_APPROXIMATION_CODES: &[&str] = &[
    "DOCX_FONT_SUBSTITUTED",
    "DOCX_FIGURE_RASTER_PLACEHOLDER",
    "DOCX_BLOCK_TALLER_THAN_PAGE",
    "DOCX_PAGINATION_BLOCK_GRANULAR",
    "DOCX_SECTION_COLUMNS_UNSUPPORTED",
    "DOCX_SECTION_DEFAULTED",
    "DOCX_SECTION_GUTTER_IGNORED",
    "DOCX_SECTION_GEOMETRY_DEFAULTED",
    "DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED",
    "DOCX_HEADER_FOOTER_OVERLAPS_BODY",
    "DOCX_HEADER_FOOTER_OUTSIDE_PAGE",
    "DOCX_HEADER_FOOTER_UNRESOLVED",
    "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED",
    "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED",
    "DOCX_SHAPE_VISUAL_OMITTED",
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

fn warning_applies_to_page(
    document: &Document,
    warning: &Diagnostic,
    page_number: Option<u32>,
) -> bool {
    let Some(page_number) = page_number else {
        return true;
    };
    if let Some(page) = warning.page {
        return page == page_number;
    }
    if let Some(object) = &warning.object {
        return document
            .blocks
            .iter()
            .any(|block| &block.id == object && block.occupies_page(page_number));
    }
    true
}

pub fn page_fidelity(document: &Document, page_number: Option<u32>) -> PageFidelity {
    let codes: BTreeSet<&str> = document
        .warnings
        .iter()
        .filter(|warning| warning_applies_to_page(document, warning, page_number))
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

#[derive(Clone, Copy)]
enum WarningScope {
    Document,
    Page(u32),
    Object,
}

struct WarningIndex<'a> {
    warnings: Vec<&'a Diagnostic>,
    by_object: BTreeMap<ObjectId, Vec<usize>>,
    by_page: BTreeMap<u32, Vec<usize>>,
    objectless: Vec<usize>,
    text_loss_objects: BTreeSet<&'a ObjectId>,
    geometry_warning_objects: BTreeSet<&'a ObjectId>,
    resource_loss_objects: BTreeSet<&'a ObjectId>,
    resource_reason_codes: BTreeSet<String>,
}

impl<'a> WarningIndex<'a> {
    fn new(doc: &'a Document) -> Self {
        let mut by_object: BTreeMap<ObjectId, Vec<usize>> = BTreeMap::new();
        let mut by_page: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        let mut objectless = Vec::new();
        let mut text_loss_objects = BTreeSet::new();
        let mut geometry_warning_objects = BTreeSet::new();
        let mut resource_loss_objects = BTreeSet::new();
        let mut resource_reason_codes = BTreeSet::new();
        for (index, warning) in doc.warnings.iter().enumerate() {
            if let Some(object) = &warning.object {
                by_object.entry(object.clone()).or_default().push(index);
                if TEXT_LOSS_CODES.contains(&warning.code.as_str()) {
                    text_loss_objects.insert(object);
                }
                if GEOMETRY_WARNING_CODES.contains(&warning.code.as_str()) {
                    geometry_warning_objects.insert(object);
                }
                if RESOURCE_LOSS_CODES.contains(&warning.code.as_str()) {
                    resource_loss_objects.insert(object);
                }
            } else {
                objectless.push(index);
            }
            if let Some(page) = warning.page {
                by_page.entry(page).or_default().push(index);
            }
            if RESOURCE_LOSS_CODES.contains(&warning.code.as_str()) {
                resource_reason_codes.insert(warning.code.clone());
            }
        }
        Self {
            warnings: doc.warnings.iter().collect(),
            by_object,
            by_page,
            objectless,
            text_loss_objects,
            geometry_warning_objects,
            resource_loss_objects,
            resource_reason_codes,
        }
    }

    fn object_warnings(&self, object: &ObjectId) -> impl Iterator<Item = &Diagnostic> + '_ {
        self.by_object
            .get(object)
            .into_iter()
            .flatten()
            .copied()
            .map(|index| self.warnings[index])
    }

    fn relevant(
        &self,
        block_ids: &BTreeSet<&ObjectId>,
        section_ids: &BTreeSet<&ObjectId>,
        pages: &BTreeSet<u32>,
        scope: WarningScope,
    ) -> Vec<&Diagnostic> {
        if matches!(scope, WarningScope::Document) {
            return self.warnings.clone();
        }
        let mut indexes = BTreeSet::new();
        indexes.extend(self.objectless.iter().copied());
        for object in block_ids.iter().chain(section_ids.iter()) {
            if let Some(entries) = self.by_object.get(*object) {
                indexes.extend(entries.iter().copied());
            }
        }
        for page in pages {
            if let Some(entries) = self.by_page.get(page) {
                indexes.extend(entries.iter().copied());
            }
        }
        indexes
            .into_iter()
            .filter(|index| {
                self.warnings[*index]
                    .page
                    .is_none_or(|page| pages.contains(&page))
            })
            .filter_map(|index| self.warnings.get(index).copied())
            .collect()
    }
}

struct FidelityContext<'a> {
    warnings: WarningIndex<'a>,
    section_by_block: BTreeMap<ObjectId, ObjectId>,
}

impl<'a> FidelityContext<'a> {
    fn new(doc: &'a Document) -> Self {
        let mut section_by_block = BTreeMap::new();
        if let Ok(ranges) = doc.section_block_ranges() {
            for (section, range) in doc.sections.iter().zip(ranges) {
                for block in &doc.blocks[range] {
                    section_by_block.insert(block.id.clone(), section.id.clone());
                }
            }
        }
        Self {
            warnings: WarningIndex::new(doc),
            section_by_block,
        }
    }

    fn section_ids(&self, blocks: &[&Block]) -> BTreeSet<&ObjectId> {
        blocks
            .iter()
            .filter_map(|block| self.section_by_block.get(&block.id))
            .collect()
    }
}

fn measured_fidelity(doc: &Document, inputs: &FidelityInputs<'_>) -> FidelityProfile {
    let context = FidelityContext::new(doc);
    measured_fidelity_with_context(doc, inputs, &context, WarningScope::Object)
}

fn measured_fidelity_with_context(
    doc: &Document,
    inputs: &FidelityInputs<'_>,
    context: &FidelityContext<'_>,
    scope: WarningScope,
) -> FidelityProfile {
    let total = inputs.blocks.len();
    let block_ids: BTreeSet<&ObjectId> = inputs.blocks.iter().map(|block| &block.id).collect();
    let pages: BTreeSet<u32> = match scope {
        WarningScope::Page(page) => BTreeSet::from([page]),
        WarningScope::Object | WarningScope::Document => inputs
            .blocks
            .iter()
            .flat_map(|block| block.fragments().map(|(page, _)| page))
            .collect(),
    };
    let section_ids = context.section_ids(inputs.blocks);
    let relevant_warnings = context
        .warnings
        .relevant(&block_ids, &section_ids, &pages, scope);
    let reasons: BTreeSet<String> = relevant_warnings
        .iter()
        .map(|warning| warning.code.clone())
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
            ) && context.warnings.text_loss_objects.contains(&block.id)
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
    let geometry_affected = inputs
        .blocks
        .iter()
        .filter(|block| {
            context
                .warnings
                .geometry_warning_objects
                .contains(&block.id)
        })
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
            context
                .warnings
                .object_warnings(&block.id)
                .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
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
    pub kind: EvidenceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<ObjectId>,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_order: Option<u32>,
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    TableCell,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
    Hyperlink,
}

impl EvidenceKind {
    pub fn of(object: &DocumentObject<'_>) -> Self {
        match object {
            DocumentObject::Block { block, .. } => block.kind.into(),
            DocumentObject::TableCell { .. } => Self::TableCell,
            DocumentObject::Overlay(overlay) => overlay.kind.into(),
            DocumentObject::Hyperlink(_) => Self::Hyperlink,
        }
    }
}

impl From<BlockKind> for EvidenceKind {
    fn from(kind: BlockKind) -> Self {
        match kind {
            BlockKind::Paragraph => Self::Paragraph,
            BlockKind::Heading => Self::Heading,
            BlockKind::ListItem => Self::ListItem,
            BlockKind::Table => Self::Table,
            BlockKind::Figure => Self::Figure,
            BlockKind::Shape => Self::Shape,
            BlockKind::Note => Self::Note,
            BlockKind::Unknown => Self::Unknown,
        }
    }
}

impl From<OverlayKind> for EvidenceKind {
    fn from(kind: OverlayKind) -> Self {
        match kind {
            OverlayKind::Header => Self::Header,
            OverlayKind::Footer => Self::Footer,
            OverlayKind::Watermark => Self::Watermark,
            OverlayKind::CommentMarker => Self::CommentMarker,
            OverlayKind::Annotation => Self::Annotation,
        }
    }
}

pub fn compute_evidence(
    doc: &Document,
    source: &DocumentSource,
    object_id: &ObjectId,
    render_fingerprint: Option<String>,
    glyph_coverage: f32,
) -> Result<EvidenceRecord, DocsightError> {
    let object =
        doc.resolve_object(object_id.as_str())
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: object_id.to_string(),
            })?;
    let anchor = object.anchor_block();
    let anchor_blocks: Vec<&Block> = anchor.into_iter().collect();

    let fidelity = measured_fidelity(
        doc,
        &FidelityInputs {
            blocks: &anchor_blocks,
            glyph_coverage,
            single_block_confidence: Some(object.confidence()),
        },
    );

    let full_text = object.text();
    let text_fragment = truncate_at_char_boundary(&full_text, 300);
    let source_span = object.source();
    let confidence = object.confidence();

    Ok(EvidenceRecord {
        document_digest: source.sha256().to_owned(),
        object_id: object.id().clone(),
        kind: EvidenceKind::of(&object),
        container_id: object.container().map(|container| container.id.clone()),
        source_path: source_span.path.clone(),
        source_offset: source_span.offset,
        page: object.page(),
        bbox: object.bbox(),
        z_index: anchor.map(|block| block.z_index),
        reading_order: object.reading_order(),
        confidence: if confidence < 0.999 {
            Some(confidence)
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

struct PageCoverageInput<'a> {
    doc: &'a Document,
    context: &'a FidelityContext<'a>,
    page: u32,
    page_blocks: &'a [&'a Block],
    glyph_coverage: f32,
    include_regions: bool,
    is_global: bool,
}

fn page_coverage(
    input: PageCoverageInput<'_>,
    accumulator: &mut CoverageAccumulator<'_>,
) -> PageCoverage {
    let PageCoverageInput {
        doc,
        context,
        page,
        page_blocks,
        glyph_coverage,
        include_regions,
        is_global,
    } = input;
    let affected_ids = &mut *accumulator.affected_ids;
    let all_reason_codes = &mut *accumulator.reason_codes;
    let fidelity = measured_fidelity_with_context(
        doc,
        &FidelityInputs {
            blocks: page_blocks,
            glyph_coverage,
            single_block_confidence: None,
        },
        context,
        if is_global {
            WarningScope::Document
        } else {
            WarningScope::Page(page)
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
                    bbox: region_bbox(block, page, is_global),
                    status: CoverageStatus::Unsupported,
                    reason_code: "DOCX_BODY_ELEMENT_UNSUPPORTED".to_owned(),
                    description:
                        "element preserved as an opaque node without semantic interpretation"
                            .to_owned(),
                });
            }
        }
        if block.kind == BlockKind::Figure
            && context
                .warnings
                .object_warnings(&block.id)
                .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
        {
            affected_ids.insert(block.id.to_string());
            all_reason_codes.insert("DOCX_FIGURE_RASTER_PLACEHOLDER".to_owned());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: region_bbox(block, page, is_global),
                    status: CoverageStatus::Unsupported,
                    reason_code: "DOCX_FIGURE_RASTER_PLACEHOLDER".to_owned(),
                    description: "embedded image bytes are not rasterized; visual evidence is a placeholder box"
                        .to_owned(),
                });
            }
        }
        if context
            .warnings
            .object_warnings(&block.id)
            .any(|warning| TEXT_LOSS_CODES.contains(&warning.code.as_str()))
        {
            affected_ids.insert(block.id.to_string());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: region_bbox(block, page, is_global),
                    status: CoverageStatus::Approximated,
                    reason_code: "DOCX_RUN_ELEMENT_UNSUPPORTED".to_owned(),
                    description:
                        "paragraph text may be incomplete because run content was not extracted"
                            .to_owned(),
                });
            }
        }
        for warning in context
            .warnings
            .object_warnings(&block.id)
            .filter(|warning| GEOMETRY_WARNING_CODES.contains(&warning.code.as_str()))
        {
            affected_ids.insert(block.id.to_string());
            all_reason_codes.insert(warning.code.clone());
            if include_regions {
                regions.push(CoverageRegion {
                    page,
                    object_id: Some(block.id.clone()),
                    bbox: region_bbox(block, page, is_global),
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
                    bbox: region_bbox(block, page, is_global),
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
            "DOCX_WIDOW_CONTROL_RELAXED",
            "DOCX_SECTION_COLUMNS_UNSUPPORTED",
            "DOCX_SECTION_DEFAULTED",
            "DOCX_SECTION_GUTTER_IGNORED",
            "DOCX_SECTION_GEOMETRY_DEFAULTED",
            "DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED",
            "DOCX_HEADER_FOOTER_OVERLAPS_BODY",
            "DOCX_HEADER_FOOTER_OUTSIDE_PAGE",
            "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED",
            "DOCX_SHAPE_VISUAL_OMITTED",
        ],
    );
    let visual_reasons = metric_reason_codes(
        &fidelity,
        &[
            "DOCX_FONT_SUBSTITUTED",
            "APPROXIMATED_PDF_FONT",
            "PDF_XOBJECT_PLACEHOLDER",
            "DOCX_FIGURE_RASTER_PLACEHOLDER",
            "DOCX_HEADER_FOOTER_UNRESOLVED",
            "DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED",
            "DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED",
            "DOCX_SHAPE_VISUAL_OMITTED",
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
        resource: resource_coverage(doc, context, page, page_blocks, is_global),
        overall_fidelity: fidelity.overall(),
        regions,
    }
}

fn resource_coverage(
    doc: &Document,
    context: &FidelityContext<'_>,
    page: u32,
    page_blocks: &[&Block],
    is_global: bool,
) -> CoverageMetric {
    let lost_objects = &context.warnings.resource_loss_objects;
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

    let reasons: Vec<String> = if is_global {
        context
            .warnings
            .resource_reason_codes
            .iter()
            .cloned()
            .collect()
    } else {
        doc.warnings
            .iter()
            .filter(|warning| RESOURCE_LOSS_CODES.contains(&warning.code.as_str()))
            .filter(|warning| match (warning.page, warning.object.as_ref()) {
                (Some(affected_page), _) => affected_page == page,
                (None, Some(object)) => page_blocks.iter().any(|block| &block.id == object),
                (None, None) => true,
            })
            .map(|warning| warning.code.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };

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
    let context = FidelityContext::new(doc);
    let mut blocks_by_page: BTreeMap<u32, Vec<&Block>> = BTreeMap::new();
    for block in &doc.blocks {
        let mut occupied = BTreeSet::new();
        for (page, _) in block.fragments() {
            if occupied.insert(page) {
                blocks_by_page.entry(page).or_default().push(block);
            }
        }
    }

    if page_filter == Some(0) {
        return Err(DocsightError::InvalidArgument {
            message: "page numbers are 1-based".to_owned(),
        });
    }
    if let Some(page) = page_filter
        && !doc.pages.iter().any(|candidate| candidate.number == page)
    {
        return Err(DocsightError::ObjectNotFound {
            object: format!("page {page}"),
        });
    }

    let pages_to_process: Vec<u32> = match page_filter {
        Some(page) => vec![page],
        None => doc.pages.iter().map(|page| page.number).collect(),
    };

    let mut page_coverages = Vec::with_capacity(pages_to_process.len());
    for page in pages_to_process {
        let page_blocks = blocks_by_page
            .get(&page)
            .map(Vec::as_slice)
            .unwrap_or_default();
        page_coverages.push(page_coverage(
            PageCoverageInput {
                doc,
                context: &context,
                page,
                page_blocks,
                glyph_coverage,
                include_regions,
                is_global: false,
            },
            &mut CoverageAccumulator {
                affected_ids: &mut affected_ids,
                reason_codes: &mut all_reason_codes,
            },
        ));
    }

    let all_blocks: Vec<&Block> = doc.blocks.iter().collect();
    let global = page_coverage(
        PageCoverageInput {
            doc,
            context: &context,
            page: 0,
            page_blocks: &all_blocks,
            glyph_coverage,
            include_regions: false,
            is_global: true,
        },
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

fn region_bbox(block: &Block, page: u32, is_global: bool) -> Option<Rect> {
    if is_global {
        block.bbox
    } else {
        block.bbox_on_page(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlockContent, DocumentMetadata, FigureBlock, IrVersion, LayoutFlags, ParagraphFormat,
        SourceSpan, TrackedChanges,
    };

    #[test]
    fn page_fidelity_excludes_warnings_owned_by_other_pages() {
        let mut warning = Diagnostic::warning(
            "DOCX_HEADER_FOOTER_OVERLAPS_BODY",
            "header overlaps page 2".to_owned(),
            "page 2 geometry is approximate",
        );
        warning.page = Some(2);
        let document = Document {
            version: IrVersion::default(),
            id: "doc_test".to_owned(),
            sha256: "0".repeat(64),
            format: DocumentFormat::Docx,
            size_bytes: 1,
            metadata: DocumentMetadata::default(),
            styles: Vec::new(),
            sections: Vec::new(),
            pages: Vec::new(),
            blocks: Vec::new(),
            resources: Vec::new(),
            links: Vec::new(),
            comments: Vec::new(),
            tracked_changes: TrackedChanges::default(),
            warnings: vec![warning],
        };

        assert!(page_fidelity(&document, Some(1)).authoritative);
        assert!(!page_fidelity(&document, Some(2)).authoritative);
        assert!(!page_fidelity(&document, None).authoritative);
    }

    #[test]
    fn resource_reasons_and_indexed_warnings_stay_on_their_page() {
        let figure = |page| Block {
            id: ObjectId::from_raw(format!("figure_{page}")),
            kind: BlockKind::Figure,
            page: Some(page),
            bbox: Some(Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 10.0,
            }),
            z_index: 0,
            reading_order: 0,
            source: SourceSpan::new(format!("figure_{page}")),
            confidence: 1.0,
            flags: LayoutFlags::default(),
            format: ParagraphFormat::default(),
            continuations: Vec::new(),
            content: BlockContent::Figure(FigureBlock {
                alt_text: None,
                caption: None,
                resource_id: None,
                width_pt: None,
                height_pt: None,
            }),
        };
        let mut warning = Diagnostic::warning(
            "DOCX_FIGURE_RASTER_PLACEHOLDER",
            "page 2 image is not decoded".to_owned(),
            "page 2 visual evidence is incomplete",
        );
        warning.page = Some(2);
        warning.object = Some(ObjectId::from_raw("figure_2"));
        let document = Document {
            version: IrVersion::default(),
            id: "doc_test".to_owned(),
            sha256: "0".repeat(64),
            format: DocumentFormat::Docx,
            size_bytes: 1,
            metadata: DocumentMetadata::default(),
            styles: Vec::new(),
            sections: Vec::new(),
            pages: Vec::new(),
            blocks: vec![figure(1), figure(2)],
            resources: Vec::new(),
            links: Vec::new(),
            comments: Vec::new(),
            tracked_changes: TrackedChanges::default(),
            warnings: vec![warning],
        };
        let context = FidelityContext::new(&document);
        let page_one = resource_coverage(&document, &context, 1, &[&document.blocks[0]], false);
        let page_two = resource_coverage(&document, &context, 2, &[&document.blocks[1]], false);
        assert!(page_one.reason_codes.is_empty());
        assert_eq!(page_one.score, 1.0);
        assert_eq!(page_two.reason_codes, ["DOCX_FIGURE_RASTER_PLACEHOLDER"]);
        assert_eq!(page_two.score, 0.0);
        let ids = BTreeSet::from([&document.blocks[0].id]);
        let indexed = context.warnings.relevant(
            &ids,
            &BTreeSet::new(),
            &BTreeSet::from([1]),
            WarningScope::Page(1),
        );
        assert!(indexed.is_empty());
        let empty_blocks: [&Block; 0] = [];
        let inputs = FidelityInputs {
            blocks: &empty_blocks,
            glyph_coverage: 1.0,
            single_block_confidence: None,
        };
        let empty_page =
            measured_fidelity_with_context(&document, &inputs, &context, WarningScope::Page(1));
        assert!(empty_page.reasons.is_empty());
        let shared_inputs = FidelityInputs {
            blocks: &[&document.blocks[0], &document.blocks[1]],
            glyph_coverage: 1.0,
            single_block_confidence: None,
        };
        let scoped_page = measured_fidelity_with_context(
            &document,
            &shared_inputs,
            &context,
            WarningScope::Page(1),
        );
        assert!(scoped_page.reasons.is_empty());
        let global =
            measured_fidelity_with_context(&document, &inputs, &context, WarningScope::Document);
        assert_eq!(global.reasons, ["DOCX_FIGURE_RASTER_PLACEHOLDER"]);
    }
}
