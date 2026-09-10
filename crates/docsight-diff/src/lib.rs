use docsight_core::{
    Block, Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, Rect,
    table_to_tsv_string,
};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_render::{RenderRequest, RenderTarget, encode_png, render_document};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Moved,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "added"),
            Self::Removed => write!(f, "removed"),
            Self::Modified => write!(f, "modified"),
            Self::Moved => write!(f, "moved"),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeCounter {
    pub added: u32,
    pub removed: u32,
    pub modified: u32,
    pub moved: u32,
}

impl ChangeCounter {
    pub fn total(&self) -> u32 {
        self.added
            .saturating_add(self.removed)
            .saturating_add(self.modified)
            .saturating_add(self.moved)
    }

    pub fn format_images(&self) -> String {
        format!(
            "+{} / -{} / changed {} / moved {}",
            self.added, self.removed, self.modified, self.moved
        )
    }

    pub fn format_tables(&self) -> String {
        if self.added > 0 || self.removed > 0 {
            format!(
                "+{} / -{} / modified {} / moved {}",
                self.added, self.removed, self.modified, self.moved
            )
        } else {
            format!("{} modified / {} moved", self.modified, self.moved)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageRecord {
    pub id: String,
    pub status: LineageStatus,
    pub target_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_object: Option<String>,
    pub match_score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChangeKind>,
    pub evidence: Vec<LineageEvidence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<LineageCandidate>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageStatus {
    Matched,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageEvidenceKind {
    NormalizedText,
    SourcePath,
    Style,
    Geometry,
    TableShape,
    ImageDigest,
    Neighborhood,
    ObjectConfidence,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageEvidence {
    pub kind: LineageEvidenceKind,
    pub score: f32,
    pub weight: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageCandidate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_object: Option<String>,
    pub match_score: f32,
    pub evidence: Vec<LineageEvidence>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Before,
    After,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffEvidence {
    pub side: DiffSide,
    pub code: String,
    pub message: String,
    pub effect: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticChangeRecord {
    pub kind: ChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    pub target_type: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<LineageRecord>,
    pub authoritative: bool,
    pub evidence: Vec<DiffEvidence>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageDiff {
    pub added_parts: Vec<String>,
    pub removed_parts: Vec<String>,
    pub modified_parts: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SemanticDiff {
    pub total_changes: u32,
    pub headings: ChangeCounter,
    pub paragraphs: ChangeCounter,
    pub tables: ChangeCounter,
    pub images: ChangeCounter,
    pub records: Vec<SemanticChangeRecord>,
    pub lineage: Vec<LineageRecord>,
    pub lineage_ambiguous: u32,
    pub evidence_limited_changes: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageVisualDiff {
    pub page: u32,
    pub authoritative: bool,
    pub evidence_status: VisualDiffEvidenceStatus,
    pub reason_codes: Vec<String>,
    pub changed_pixels: u32,
    pub total_pixels: u32,
    pub change_fraction: f32,
    pub changed_regions: Vec<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<VisualDiffArtifact>,
    #[serde(skip_serializing)]
    pub diff_png: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VisualDiffArtifact {
    pub relative_path: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualDiffEvidenceStatus {
    #[default]
    Exact,
    EvidenceLimited,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VisualDiff {
    pub authoritative: bool,
    pub evidence_status: VisualDiffEvidenceStatus,
    pub reason_codes: Vec<String>,
    pub dpi: u16,
    pub threshold: u8,
    pub pages_before: u32,
    pub pages_after: u32,
    pub layout_changed_pages: u32,
    pub layout_regression_score: f32,
    pub largest_drift_pt: Option<f32>,
    pub largest_drift_page: Option<u32>,
    pub page_diffs: Vec<PageVisualDiff>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub pages_before: u32,
    pub pages_after: u32,
    pub semantic_changes: u32,
    pub layout_changed_pages: u32,
    pub largest_drift_pt: Option<f32>,
    pub largest_drift_page: Option<u32>,
    pub images: ChangeCounter,
    pub tables: ChangeCounter,
    pub lineage_ambiguous: u32,
    pub evidence_limited_changes: u32,
    pub warnings: Vec<String>,
}

impl DiffSummary {
    pub fn format_summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "{:<18} {} → {}",
            "Pages", self.pages_before, self.pages_after
        ));
        lines.push(format!(
            "{:<18} {}",
            "Semantic changes", self.semantic_changes
        ));
        lines.push(format!(
            "{:<18} {} pages",
            "Layout changes", self.layout_changed_pages
        ));
        if let (Some(drift), Some(page)) = (self.largest_drift_pt, self.largest_drift_page) {
            lines.push(format!(
                "{:<18} page {}, {:.1} pt vertical",
                "Largest drift", page, drift
            ));
        }
        lines.push(format!("{:<18} {}", "Images", self.images.format_images()));
        lines.push(format!("{:<18} {}", "Tables", self.tables.format_tables()));
        if self.lineage_ambiguous > 0 {
            lines.push(format!(
                "{:<18} {}",
                "Lineage ambiguous", self.lineage_ambiguous
            ));
        }
        if self.evidence_limited_changes > 0 {
            lines.push(format!(
                "{:<18} {}",
                "Evidence-limited", self.evidence_limited_changes
            ));
        }
        for w in &self.warnings {
            lines.push(format!("{:<18} {}", "Warnings", w));
        }
        lines.join("\n")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffDocumentIdentity {
    pub id: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentDiffResult {
    pub format_before: DocumentFormat,
    pub format_after: DocumentFormat,
    pub before_document: DiffDocumentIdentity,
    pub after_document: DiffDocumentIdentity,
    pub package: PackageDiff,
    pub semantic: SemanticDiff,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual: Option<VisualDiff>,
    pub summary: DiffSummary,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Clone, Debug)]
pub struct DiffOptions {
    pub visual: bool,
    pub dpi: u16,
    pub threshold: u8,
    pub out_dir: Option<std::path::PathBuf>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            visual: false,
            dpi: 144,
            threshold: 8,
            out_dir: None,
        }
    }
}

pub fn diff_documents(
    source_before: &DocumentSource,
    source_after: &DocumentSource,
    options: &DiffOptions,
) -> Result<DocumentDiffResult, DocsightError> {
    let doc_before = load_doc(source_before)?;
    let doc_after = load_doc(source_after)?;

    let package = diff_package(source_before, source_after)?;
    let semantic = diff_semantic(&doc_before, &doc_after)?;

    let pages_before = doc_before.pages.len() as u32;
    let pages_after = doc_after.pages.len() as u32;

    let visual = if options.visual || options.out_dir.is_some() {
        Some(diff_visual(
            source_before,
            source_after,
            &doc_before,
            &doc_after,
            options.dpi,
            options.threshold,
            options.out_dir.as_deref(),
        )?)
    } else {
        None
    };

    let layout_changed_pages = visual
        .as_ref()
        .map(|v| v.layout_changed_pages)
        .unwrap_or_else(|| {
            if pages_before != pages_after {
                pages_before.abs_diff(pages_after)
            } else {
                0
            }
        });

    let (largest_drift_pt, largest_drift_page) = if let Some(ref v) = visual {
        (v.largest_drift_pt, v.largest_drift_page)
    } else {
        calculate_largest_drift(&doc_before, &doc_after)
    };

    let mut combined_warnings = Vec::new();
    combined_warnings.extend(doc_before.warnings);
    combined_warnings.extend(doc_after.warnings);
    combined_warnings.extend(
        semantic
            .lineage
            .iter()
            .filter_map(lineage_ambiguity_warning),
    );
    if let Some(visual) = &visual
        && !visual.authoritative
    {
        combined_warnings.push(visual_diff_evidence_warning(&visual.reason_codes));
    }

    let summary_warnings: Vec<String> = combined_warnings
        .iter()
        .map(|w| format!("{}: {}", w.code, w.message))
        .collect();

    let summary = DiffSummary {
        pages_before,
        pages_after,
        semantic_changes: semantic.total_changes,
        layout_changed_pages,
        largest_drift_pt,
        largest_drift_page,
        images: semantic.images.clone(),
        tables: semantic.tables.clone(),
        lineage_ambiguous: semantic.lineage_ambiguous,
        evidence_limited_changes: semantic.evidence_limited_changes,
        warnings: summary_warnings,
    };

    Ok(DocumentDiffResult {
        format_before: source_before.format(),
        format_after: source_after.format(),
        before_document: DiffDocumentIdentity {
            id: doc_before.id,
            sha256: doc_before.sha256,
        },
        after_document: DiffDocumentIdentity {
            id: doc_after.id,
            sha256: doc_after.sha256,
        },
        package,
        semantic,
        visual,
        summary,
        warnings: combined_warnings,
    })
}

fn visual_diff_evidence_warning(reason_codes: &[String]) -> Diagnostic {
    Diagnostic {
        code: "DIFF_VISUAL_EVIDENCE_LIMITED".to_owned(),
        severity: docsight_core::DiagnosticSeverity::Warning,
        message: format!(
            "visual diff uses deterministic approximated rendering affected by {}",
            reason_codes.join(", ")
        ),
        effect: "changed pixels and regions are valid for the DOCSIGHT render profile but are not source-faithful visual evidence"
            .to_owned(),
        object: None,
        page: None,
    }
}

fn lineage_ambiguity_warning(record: &LineageRecord) -> Option<Diagnostic> {
    if record.status != LineageStatus::Ambiguous {
        return None;
    }
    let object = record
        .before_object
        .as_ref()
        .or(record.after_object.as_ref())
        .map(|id| docsight_core::ObjectId::from_raw(id.clone()));
    Some(Diagnostic {
        code: "DIFF_LINEAGE_AMBIGUOUS".to_owned(),
        severity: docsight_core::DiagnosticSeverity::Warning,
        message: format!(
            "{} lineage has {} competing correspondence candidates",
            record.target_type,
            record.candidates.len()
        ),
        effect: "the related semantic additions or removals are not authoritative until the correspondence is resolved"
            .to_owned(),
        object,
        page: None,
    })
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

pub fn diff_package(
    source_before: &DocumentSource,
    source_after: &DocumentSource,
) -> Result<PackageDiff, DocsightError> {
    if source_before.format() == DocumentFormat::Docx
        && source_after.format() == DocumentFormat::Docx
    {
        let parts_before = inspect_zip_parts(source_before.bytes())?;
        let parts_after = inspect_zip_parts(source_after.bytes())?;

        let mut added_parts = Vec::new();
        let mut removed_parts = Vec::new();
        let mut modified_parts = Vec::new();

        for (name, (size_after, crc_after)) in &parts_after {
            if let Some((size_before, crc_before)) = parts_before.get(name) {
                if size_after != size_before || crc_after != crc_before {
                    modified_parts.push(name.clone());
                }
            } else {
                added_parts.push(name.clone());
            }
        }

        for name in parts_before.keys() {
            if !parts_after.contains_key(name) {
                removed_parts.push(name.clone());
            }
        }

        added_parts.sort();
        removed_parts.sort();
        modified_parts.sort();

        Ok(PackageDiff {
            added_parts,
            removed_parts,
            modified_parts,
        })
    } else {
        let mut modified_parts = Vec::new();
        if source_before.sha256() != source_after.sha256() {
            modified_parts.push("document/content".to_owned());
        }
        Ok(PackageDiff {
            added_parts: Vec::new(),
            removed_parts: Vec::new(),
            modified_parts,
        })
    }
}

fn inspect_zip_parts(bytes: &[u8]) -> Result<BTreeMap<String, (u64, u32)>, DocsightError> {
    let reader = Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|error| DocsightError::MalformedDocument {
            message: format!("ZIP package inspection error: {error}"),
        })?;

    let mut parts = BTreeMap::new();
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|error| DocsightError::MalformedDocument {
                message: format!("ZIP entry read error: {error}"),
            })?;
        if !file.is_dir() {
            parts.insert(file.name().to_owned(), (file.size(), file.crc32()));
        }
    }
    Ok(parts)
}

const TEXT_SIMILARITY_THRESHOLD: f32 = 0.6;
const STRUCTURAL_SIMILARITY_THRESHOLD: f32 = 0.5;
const AMBIGUITY_MARGIN: f32 = 0.05;

#[derive(Clone, Default)]
struct Neighborhood {
    previous: Option<String>,
    next: Option<String>,
}

#[derive(Clone)]
struct AlignItem {
    document_sha256: String,
    object_id: String,
    page: Option<u32>,
    bbox: Option<Rect>,
    source_path: String,
    style_id: Option<String>,
    table_shape: Option<(u32, u32)>,
    image_digest: Option<String>,
    confidence: f32,
    neighborhood: Neighborhood,
    key: String,
    display: String,
}

#[derive(Clone)]
struct MatchedPair {
    before_index: usize,
    after_index: usize,
    exact: bool,
    score: f32,
    evidence: Vec<LineageEvidence>,
}

#[derive(Clone)]
struct CandidatePair {
    before_index: usize,
    after_index: usize,
    exact: bool,
    score: f32,
    evidence: Vec<LineageEvidence>,
}

struct Alignment {
    pairs: Vec<MatchedPair>,
    ambiguities: Vec<AlignmentAmbiguity>,
}

struct AlignmentAmbiguity {
    anchor: AmbiguityAnchor,
    candidates: Vec<CandidatePair>,
}

enum AmbiguityAnchor {
    Before(usize),
    After(usize),
}

struct DiffContext<'a> {
    target_type: &'a str,
    before_warnings: &'a [Diagnostic],
    after_warnings: &'a [Diagnostic],
}

struct AlignMetadata {
    style_id: Option<String>,
    table_shape: Option<(u32, u32)>,
    image_digest: Option<String>,
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_set(text: &str) -> std::collections::BTreeSet<String> {
    normalize_text(text)
        .split(' ')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn text_similarity(before: &str, after: &str) -> f32 {
    let before_tokens = token_set(before);
    let after_tokens = token_set(after);
    if before_tokens.is_empty() && after_tokens.is_empty() {
        return 1.0;
    }
    if before_tokens.is_empty() || after_tokens.is_empty() {
        return 0.0;
    }
    let intersection = before_tokens.intersection(&after_tokens).count() as f32;
    let union = before_tokens.union(&after_tokens).count() as f32;
    intersection / union
}

fn align_items(before: &[AlignItem], after: &[AlignItem]) -> Alignment {
    let mut candidates = Vec::new();
    for (before_index, before_item) in before.iter().enumerate() {
        for (after_index, after_item) in after.iter().enumerate() {
            if let Some(candidate) =
                candidate_pair(before_index, after_index, before_item, after_item)
            {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| {
                before[left.before_index]
                    .object_id
                    .cmp(&before[right.before_index].object_id)
            })
            .then_with(|| {
                after[left.after_index]
                    .object_id
                    .cmp(&after[right.after_index].object_id)
            })
    });

    let mut before_ambiguous = BTreeMap::new();
    for before_index in 0..before.len() {
        let contenders = competing_candidates(&candidates, |candidate| {
            candidate.before_index == before_index
        });
        if contenders.len() > 1 {
            before_ambiguous.insert(before_index, contenders);
        }
    }

    let mut after_ambiguous = BTreeMap::new();
    for after_index in 0..after.len() {
        let contenders = competing_candidates(&candidates, |candidate| {
            candidate.after_index == after_index
        });
        if contenders.len() > 1 {
            after_ambiguous.insert(after_index, contenders);
        }
    }

    let ambiguous_before: std::collections::BTreeSet<usize> =
        before_ambiguous.keys().copied().collect();
    let ambiguous_after: std::collections::BTreeSet<usize> =
        after_ambiguous.keys().copied().collect();
    let mut before_matched = vec![false; before.len()];
    let mut after_matched = vec![false; after.len()];
    let mut pairs = Vec::new();
    for candidate in &candidates {
        if ambiguous_before.contains(&candidate.before_index)
            || ambiguous_after.contains(&candidate.after_index)
            || before_matched[candidate.before_index]
            || after_matched[candidate.after_index]
        {
            continue;
        }
        before_matched[candidate.before_index] = true;
        after_matched[candidate.after_index] = true;
        pairs.push(MatchedPair {
            before_index: candidate.before_index,
            after_index: candidate.after_index,
            exact: candidate.exact,
            score: candidate.score,
            evidence: candidate.evidence.clone(),
        });
    }
    pairs.sort_by(|left, right| {
        left.after_index
            .cmp(&right.after_index)
            .then_with(|| left.before_index.cmp(&right.before_index))
    });

    let mut ambiguities = Vec::new();
    let mut covered_after = std::collections::BTreeSet::new();
    for (before_index, contenders) in before_ambiguous {
        for contender in &contenders {
            covered_after.insert(contender.after_index);
        }
        ambiguities.push(AlignmentAmbiguity {
            anchor: AmbiguityAnchor::Before(before_index),
            candidates: contenders,
        });
    }
    for (after_index, contenders) in after_ambiguous {
        if covered_after.contains(&after_index) {
            continue;
        }
        ambiguities.push(AlignmentAmbiguity {
            anchor: AmbiguityAnchor::After(after_index),
            candidates: contenders,
        });
    }
    ambiguities.sort_by(|left, right| {
        ambiguity_anchor_key(left, before, after).cmp(&ambiguity_anchor_key(right, before, after))
    });

    Alignment { pairs, ambiguities }
}

fn competing_candidates(
    candidates: &[CandidatePair],
    predicate: impl Fn(&CandidatePair) -> bool,
) -> Vec<CandidatePair> {
    let contenders: Vec<CandidatePair> = candidates
        .iter()
        .filter(|candidate| predicate(candidate))
        .cloned()
        .collect();
    let Some(best_score) = contenders.first().map(|candidate| candidate.score) else {
        return Vec::new();
    };
    contenders
        .into_iter()
        .take_while(|candidate| best_score - candidate.score <= AMBIGUITY_MARGIN)
        .collect()
}

fn candidate_pair(
    before_index: usize,
    after_index: usize,
    before: &AlignItem,
    after: &AlignItem,
) -> Option<CandidatePair> {
    let exact = before.key == after.key;
    let evidence = lineage_evidence(before, after);
    let total_weight: f32 = evidence.iter().map(|entry| entry.weight).sum();
    if total_weight <= 0.0 {
        return None;
    }
    let score = evidence
        .iter()
        .map(|entry| entry.score * entry.weight)
        .sum::<f32>()
        / total_weight;
    let threshold = if exact || before.table_shape.is_some() || before.image_digest.is_some() {
        STRUCTURAL_SIMILARITY_THRESHOLD
    } else {
        TEXT_SIMILARITY_THRESHOLD
    };
    if score < threshold {
        return None;
    }
    Some(CandidatePair {
        before_index,
        after_index,
        exact,
        score,
        evidence,
    })
}

fn lineage_evidence(before: &AlignItem, after: &AlignItem) -> Vec<LineageEvidence> {
    let mut evidence = Vec::new();
    if !before.key.is_empty() || !after.key.is_empty() {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::NormalizedText,
            score: text_similarity(&before.key, &after.key),
            weight: 0.45,
            before: Some(fingerprint_label(&before.key)),
            after: Some(fingerprint_label(&after.key)),
        });
    }
    if !before.source_path.is_empty() || !after.source_path.is_empty() {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::SourcePath,
            score: if before.source_path == after.source_path {
                1.0
            } else {
                0.0
            },
            weight: 0.15,
            before: Some(before.source_path.clone()),
            after: Some(after.source_path.clone()),
        });
    }
    if let (Some(before_style), Some(after_style)) = (&before.style_id, &after.style_id) {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::Style,
            score: if before_style == after_style {
                1.0
            } else {
                0.0
            },
            weight: 0.08,
            before: Some(before_style.clone()),
            after: Some(after_style.clone()),
        });
    }
    if let (Some(before_bbox), Some(after_bbox)) = (before.bbox, after.bbox) {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::Geometry,
            score: geometry_similarity(before.page, before_bbox, after.page, after_bbox),
            weight: 0.08,
            before: Some(geometry_label(before.page, before_bbox)),
            after: Some(geometry_label(after.page, after_bbox)),
        });
    }
    if let (Some(before_shape), Some(after_shape)) = (before.table_shape, after.table_shape) {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::TableShape,
            score: table_shape_similarity(before_shape, after_shape),
            weight: 0.25,
            before: Some(format!("{}x{}", before_shape.0, before_shape.1)),
            after: Some(format!("{}x{}", after_shape.0, after_shape.1)),
        });
    }
    if let (Some(before_digest), Some(after_digest)) = (&before.image_digest, &after.image_digest) {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::ImageDigest,
            score: if before_digest == after_digest {
                1.0
            } else {
                0.0
            },
            weight: 0.40,
            before: Some(before_digest.clone()),
            after: Some(after_digest.clone()),
        });
    }
    if let Some((score, before_value, after_value)) =
        neighborhood_similarity(&before.neighborhood, &after.neighborhood)
    {
        evidence.push(LineageEvidence {
            kind: LineageEvidenceKind::Neighborhood,
            score,
            weight: 0.09,
            before: Some(before_value),
            after: Some(after_value),
        });
    }
    evidence.push(LineageEvidence {
        kind: LineageEvidenceKind::ObjectConfidence,
        score: before.confidence.min(after.confidence).clamp(0.0, 1.0),
        weight: 0.05,
        before: Some(format!("{:.3}", before.confidence)),
        after: Some(format!("{:.3}", after.confidence)),
    });
    evidence
}

fn geometry_similarity(
    before_page: Option<u32>,
    before: Rect,
    after_page: Option<u32>,
    after: Rect,
) -> f32 {
    if before_page != after_page {
        return 0.0;
    }
    let before_center_x = (before.x0 + before.x1) / 2.0;
    let before_center_y = (before.y0 + before.y1) / 2.0;
    let after_center_x = (after.x0 + after.x1) / 2.0;
    let after_center_y = (after.y0 + after.y1) / 2.0;
    let distance = (before_center_x - after_center_x).abs()
        + (before_center_y - after_center_y).abs()
        + (before.width() - after.width()).abs()
        + (before.height() - after.height()).abs();
    (1.0 - distance / 720.0).clamp(0.0, 1.0)
}

fn table_shape_similarity(before: (u32, u32), after: (u32, u32)) -> f32 {
    let rows = before.0.min(after.0) as f32 / before.0.max(after.0).max(1) as f32;
    let columns = before.1.min(after.1) as f32 / before.1.max(after.1).max(1) as f32;
    (rows + columns) / 2.0
}

fn geometry_label(page: Option<u32>, bbox: Rect) -> String {
    let page = page.map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    format!(
        "p{page}:{:.2},{:.2},{:.2},{:.2}",
        bbox.x0, bbox.y0, bbox.x1, bbox.y1
    )
}

fn neighborhood_similarity(
    before: &Neighborhood,
    after: &Neighborhood,
) -> Option<(f32, String, String)> {
    let mut scores = Vec::new();
    if let (Some(before_previous), Some(after_previous)) = (&before.previous, &after.previous) {
        scores.push(text_similarity(before_previous, after_previous));
    }
    if let (Some(before_next), Some(after_next)) = (&before.next, &after.next) {
        scores.push(text_similarity(before_next, after_next));
    }
    if scores.is_empty() {
        return None;
    }
    let score = scores.iter().sum::<f32>() / scores.len() as f32;
    Some((score, neighborhood_label(before), neighborhood_label(after)))
}

fn neighborhood_label(neighborhood: &Neighborhood) -> String {
    let previous = neighborhood
        .previous
        .as_ref()
        .map_or_else(|| "none".to_owned(), |value| fingerprint_label(value));
    let next = neighborhood
        .next
        .as_ref()
        .map_or_else(|| "none".to_owned(), |value| fingerprint_label(value));
    format!("previous={previous};next={next}")
}

fn fingerprint_label(value: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(value.as_bytes());
    let suffix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{suffix}")
}

fn moved_exact_pairs(pairs: &[MatchedPair]) -> std::collections::BTreeSet<usize> {
    let exact: Vec<&MatchedPair> = pairs.iter().filter(|pair| pair.exact).collect();
    let before_indices: Vec<usize> = exact.iter().map(|pair| pair.before_index).collect();
    if before_indices.is_empty() {
        return std::collections::BTreeSet::new();
    }
    let mut lengths = vec![1_usize; before_indices.len()];
    let mut previous = vec![usize::MAX; before_indices.len()];
    for current in (0..before_indices.len()).rev() {
        for earlier in (current + 1)..before_indices.len() {
            if before_indices[earlier] > before_indices[current]
                && lengths[earlier] + 1 > lengths[current]
            {
                lengths[current] = lengths[earlier] + 1;
                previous[current] = earlier;
            }
        }
    }
    let mut best = 0_usize;
    for (index, length) in lengths.iter().enumerate() {
        if *length > lengths[best] {
            best = index;
        }
    }
    let mut stable = std::collections::BTreeSet::new();
    let mut current = Some(best);
    while let Some(index) = current {
        stable.insert(exact[index].after_index);
        current = if previous[index] == usize::MAX {
            None
        } else {
            Some(previous[index])
        };
    }
    pairs
        .iter()
        .enumerate()
        .filter(|(_, pair)| pair.exact && stable.contains(&pair.after_index))
        .map(|(position, _)| position)
        .collect()
}

fn lineage_id(target_type: &str, before: &AlignItem, after: &AlignItem) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(target_type.as_bytes());
    hasher.update([0]);
    hasher.update(before.document_sha256.as_bytes());
    hasher.update([0]);
    hasher.update(before.object_id.as_bytes());
    hasher.update([0]);
    hasher.update(after.document_sha256.as_bytes());
    hasher.update([0]);
    hasher.update(after.object_id.as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("lin_{suffix}")
}

fn ambiguity_id(
    target_type: &str,
    anchor: &AmbiguityAnchor,
    candidates: &[CandidatePair],
    before: &[AlignItem],
    after: &[AlignItem],
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(target_type.as_bytes());
    hasher.update([0]);
    match anchor {
        AmbiguityAnchor::Before(index) => {
            hasher.update(b"before");
            hasher.update([0]);
            hasher.update(before[*index].document_sha256.as_bytes());
            hasher.update([0]);
            hasher.update(before[*index].object_id.as_bytes());
        }
        AmbiguityAnchor::After(index) => {
            hasher.update(b"after");
            hasher.update([0]);
            hasher.update(after[*index].document_sha256.as_bytes());
            hasher.update([0]);
            hasher.update(after[*index].object_id.as_bytes());
        }
    }
    for candidate in candidates {
        hasher.update([0]);
        hasher.update(before[candidate.before_index].object_id.as_bytes());
        hasher.update([0]);
        hasher.update(after[candidate.after_index].object_id.as_bytes());
    }
    let digest = hasher.finalize();
    let suffix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("lin_{suffix}")
}

fn ambiguity_anchor_key(
    ambiguity: &AlignmentAmbiguity,
    before: &[AlignItem],
    after: &[AlignItem],
) -> (u8, String) {
    match ambiguity.anchor {
        AmbiguityAnchor::Before(index) => (0, before[index].object_id.clone()),
        AmbiguityAnchor::After(index) => (1, after[index].object_id.clone()),
    }
}

fn matched_lineage(
    target_type: &str,
    before: &AlignItem,
    after: &AlignItem,
    pair: &MatchedPair,
    kind: Option<ChangeKind>,
) -> LineageRecord {
    LineageRecord {
        id: lineage_id(target_type, before, after),
        status: LineageStatus::Matched,
        target_type: target_type.to_owned(),
        before_object: Some(before.object_id.clone()),
        after_object: Some(after.object_id.clone()),
        match_score: pair.score,
        kind,
        evidence: pair.evidence.clone(),
        candidates: Vec::new(),
    }
}

fn ambiguous_lineage(
    target_type: &str,
    ambiguity: &AlignmentAmbiguity,
    before: &[AlignItem],
    after: &[AlignItem],
) -> LineageRecord {
    let (before_object, after_object) = match ambiguity.anchor {
        AmbiguityAnchor::Before(index) => (Some(before[index].object_id.clone()), None),
        AmbiguityAnchor::After(index) => (None, Some(after[index].object_id.clone())),
    };
    let match_score = ambiguity
        .candidates
        .first()
        .map_or(0.0, |candidate| candidate.score);
    let candidates = ambiguity
        .candidates
        .iter()
        .map(|candidate| LineageCandidate {
            before_object: Some(before[candidate.before_index].object_id.clone()),
            after_object: Some(after[candidate.after_index].object_id.clone()),
            match_score: candidate.score,
            evidence: candidate.evidence.clone(),
        })
        .collect();
    LineageRecord {
        id: ambiguity_id(
            target_type,
            &ambiguity.anchor,
            &ambiguity.candidates,
            before,
            after,
        ),
        status: LineageStatus::Ambiguous,
        target_type: target_type.to_owned(),
        before_object,
        after_object,
        match_score,
        kind: None,
        evidence: ambiguity
            .candidates
            .first()
            .map_or_else(Vec::new, |candidate| candidate.evidence.clone()),
        candidates,
    }
}

fn push_change_record(
    records: &mut Vec<SemanticChangeRecord>,
    counter: &mut ChangeCounter,
    context: &DiffContext<'_>,
    kind: ChangeKind,
    before_item: Option<&AlignItem>,
    after_item: Option<&AlignItem>,
    lineage: Option<LineageRecord>,
) -> Result<(), DocsightError> {
    let (object_id, page) = match (kind, before_item, after_item) {
        (ChangeKind::Removed, Some(before), _) => (Some(before.object_id.clone()), before.page),
        (_, _, Some(after)) => (Some(after.object_id.clone()), after.page),
        (_, Some(before), _) => (Some(before.object_id.clone()), before.page),
        (_, None, None) => (None, None),
    };
    let (description, before_text, after_text) = match (before_item, after_item) {
        (Some(before), Some(after)) if kind == ChangeKind::Modified => (
            format!(
                "{target_type} modified (match score {score:.3})",
                target_type = context.target_type,
                score = lineage.as_ref().map_or(1.0, |record| record.match_score)
            ),
            Some(before.display.clone()),
            Some(after.display.clone()),
        ),
        (Some(before), Some(after)) => (
            format!(
                "{target_type} moved: was position-linked to \"{}\"",
                before.display,
                target_type = context.target_type
            ),
            Some(before.display.clone()),
            Some(after.display.clone()),
        ),
        (None, Some(after)) => (
            format!("{} added: \"{}\"", context.target_type, after.display),
            None,
            Some(after.display.clone()),
        ),
        (Some(before), None) => (
            format!("{} removed: \"{}\"", context.target_type, before.display),
            Some(before.display.clone()),
            None,
        ),
        (None, None) => (format!("{} changed", context.target_type), None, None),
    };
    increment_counter(counter, kind)?;
    let evidence = diff_evidence(before_item, after_item, context);
    let authoritative = evidence.is_empty()
        && lineage
            .as_ref()
            .is_none_or(|record| record.status == LineageStatus::Matched);
    records.push(SemanticChangeRecord {
        kind,
        object_id,
        page,
        target_type: context.target_type.to_owned(),
        description,
        before: before_text,
        after: after_text,
        lineage,
        authoritative,
        evidence,
    });
    Ok(())
}

fn increment_counter(counter: &mut ChangeCounter, kind: ChangeKind) -> Result<(), DocsightError> {
    let value = match kind {
        ChangeKind::Added => &mut counter.added,
        ChangeKind::Removed => &mut counter.removed,
        ChangeKind::Modified => &mut counter.modified,
        ChangeKind::Moved => &mut counter.moved,
    };
    *value = value
        .checked_add(1)
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "semantic diff changes".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
    Ok(())
}

fn diff_collection(
    before: &[AlignItem],
    after: &[AlignItem],
    context: &DiffContext<'_>,
    records: &mut Vec<SemanticChangeRecord>,
    lineage: &mut Vec<LineageRecord>,
) -> Result<ChangeCounter, DocsightError> {
    let mut counter = ChangeCounter::default();
    let alignment = align_items(before, after);
    let stable_positions = moved_exact_pairs(&alignment.pairs);

    let mut handled_before = vec![false; before.len()];
    let mut handled_after = vec![false; after.len()];
    let mut ambiguous_before = BTreeMap::new();
    let mut ambiguous_after = BTreeMap::new();
    for ambiguity in &alignment.ambiguities {
        let record = ambiguous_lineage(context.target_type, ambiguity, before, after);
        match ambiguity.anchor {
            AmbiguityAnchor::Before(index) => {
                ambiguous_before.insert(index, record.clone());
                for candidate in &ambiguity.candidates {
                    ambiguous_after
                        .entry(candidate.after_index)
                        .or_insert_with(|| record.clone());
                }
            }
            AmbiguityAnchor::After(index) => {
                ambiguous_after.insert(index, record.clone());
                for candidate in &ambiguity.candidates {
                    ambiguous_before
                        .entry(candidate.before_index)
                        .or_insert_with(|| record.clone());
                }
            }
        }
        lineage.push(record);
    }

    for (position, pair) in alignment.pairs.iter().enumerate() {
        handled_before[pair.before_index] = true;
        handled_after[pair.after_index] = true;
        if pair.exact && stable_positions.contains(&position) {
            lineage.push(matched_lineage(
                context.target_type,
                &before[pair.before_index],
                &after[pair.after_index],
                pair,
                None,
            ));
            continue;
        }
        let kind = if pair.exact {
            ChangeKind::Moved
        } else {
            ChangeKind::Modified
        };
        let summary_lineage = matched_lineage(
            context.target_type,
            &before[pair.before_index],
            &after[pair.after_index],
            pair,
            None,
        );
        let record_lineage = LineageRecord {
            kind: Some(kind),
            ..summary_lineage.clone()
        };
        lineage.push(summary_lineage);
        push_change_record(
            records,
            &mut counter,
            context,
            kind,
            Some(&before[pair.before_index]),
            Some(&after[pair.after_index]),
            Some(record_lineage),
        )?;
    }

    for (index, item) in after.iter().enumerate() {
        if !handled_after[index] {
            push_change_record(
                records,
                &mut counter,
                context,
                ChangeKind::Added,
                None,
                Some(item),
                ambiguous_after.get(&index).cloned(),
            )?;
        }
    }
    for (index, item) in before.iter().enumerate() {
        if !handled_before[index] {
            push_change_record(
                records,
                &mut counter,
                context,
                ChangeKind::Removed,
                Some(item),
                None,
                ambiguous_before.get(&index).cloned(),
            )?;
        }
    }

    Ok(counter)
}

pub fn diff_semantic(before: &Document, after: &Document) -> Result<SemanticDiff, DocsightError> {
    let mut records = Vec::new();
    let mut lineage = Vec::new();
    let before_neighborhoods = document_neighborhoods(before);
    let after_neighborhoods = document_neighborhoods(after);

    let heading_items =
        |document: &Document, neighborhoods: &BTreeMap<String, Neighborhood>| -> Vec<AlignItem> {
            document
                .headings()
                .map(|(block, heading)| {
                    align_item(
                        document,
                        neighborhoods,
                        block,
                        normalize_text(&format!("h{}:{}", heading.level, heading.text)),
                        heading.text.clone(),
                        AlignMetadata {
                            style_id: heading.style_id.clone(),
                            table_shape: None,
                            image_digest: None,
                        },
                    )
                })
                .collect()
        };
    let headings = diff_collection(
        &heading_items(before, &before_neighborhoods),
        &heading_items(after, &after_neighborhoods),
        &DiffContext {
            target_type: "heading",
            before_warnings: &before.warnings,
            after_warnings: &after.warnings,
        },
        &mut records,
        &mut lineage,
    )?;

    let paragraph_items =
        |document: &Document, neighborhoods: &BTreeMap<String, Neighborhood>| -> Vec<AlignItem> {
            document
                .paragraphs()
                .map(|(block, paragraph)| {
                    align_item(
                        document,
                        neighborhoods,
                        block,
                        normalize_text(&paragraph.text),
                        paragraph.text.clone(),
                        AlignMetadata {
                            style_id: paragraph.style_id.clone(),
                            table_shape: None,
                            image_digest: None,
                        },
                    )
                })
                .collect()
        };
    let paragraphs = diff_collection(
        &paragraph_items(before, &before_neighborhoods),
        &paragraph_items(after, &after_neighborhoods),
        &DiffContext {
            target_type: "paragraph",
            before_warnings: &before.warnings,
            after_warnings: &after.warnings,
        },
        &mut records,
        &mut lineage,
    )?;

    let table_items =
        |document: &Document, neighborhoods: &BTreeMap<String, Neighborhood>| -> Vec<AlignItem> {
            document
                .tables()
                .map(|(block, table)| {
                    align_item(
                        document,
                        neighborhoods,
                        block,
                        normalize_text(&table_to_tsv_string(table)),
                        format!("{}x{}", table.rows, table.columns),
                        AlignMetadata {
                            style_id: None,
                            table_shape: Some((table.rows, table.columns)),
                            image_digest: None,
                        },
                    )
                })
                .collect()
        };
    let tables = diff_collection(
        &table_items(before, &before_neighborhoods),
        &table_items(after, &after_neighborhoods),
        &DiffContext {
            target_type: "table",
            before_warnings: &before.warnings,
            after_warnings: &after.warnings,
        },
        &mut records,
        &mut lineage,
    )?;

    let figure_items = |document: &Document,
                        neighborhoods: &BTreeMap<String, Neighborhood>|
     -> Vec<AlignItem> {
        document
            .figures()
            .map(|(block, figure)| {
                let relationship = figure.resource_id.as_deref().unwrap_or("<none>");
                let digest = document
                    .resources
                    .iter()
                    .find(|resource| resource.name == relationship)
                    .and_then(|resource| resource.content_sha256.as_deref());
                let digest_label = digest.unwrap_or("<unavailable>");
                let alt_text = figure.alt_text.as_deref().unwrap_or("<none>");
                let caption = figure.caption.as_deref().unwrap_or("<none>");
                let display = match (&figure.caption, &figure.alt_text, &figure.resource_id) {
                    (Some(value), _, _) | (None, Some(value), _) | (None, None, Some(value)) => {
                        value.clone()
                    }
                    (None, None, None) => "unidentified figure".to_owned(),
                };
                align_item(
                    document,
                    neighborhoods,
                    block,
                    normalize_text(&format!(
                        "resource {relationship} digest {digest_label} alt {alt_text} caption {caption}"
                    )),
                    display,
                    AlignMetadata {
                        style_id: None,
                        table_shape: None,
                        image_digest: digest.map(str::to_owned),
                    },
                )
            })
            .collect()
    };
    let images = diff_collection(
        &figure_items(before, &before_neighborhoods),
        &figure_items(after, &after_neighborhoods),
        &DiffContext {
            target_type: "figure",
            before_warnings: &before.warnings,
            after_warnings: &after.warnings,
        },
        &mut records,
        &mut lineage,
    )?;

    records.sort_by(|left, right| {
        left.target_type.cmp(&right.target_type).then_with(|| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.object_id.cmp(&right.object_id))
                .then_with(|| left.description.cmp(&right.description))
        })
    });
    lineage.sort_by(|left, right| {
        left.target_type
            .cmp(&right.target_type)
            .then_with(|| left.status.cmp(&right.status))
            .then_with(|| left.id.cmp(&right.id))
    });

    let total_changes = headings
        .total()
        .saturating_add(paragraphs.total())
        .saturating_add(tables.total())
        .saturating_add(images.total());
    let lineage_ambiguous = u32::try_from(
        lineage
            .iter()
            .filter(|record| record.status == LineageStatus::Ambiguous)
            .count(),
    )
    .map_err(|_| DocsightError::ResourceLimit {
        resource: "lineage ambiguity records".to_owned(),
        limit: u64::from(u32::MAX),
    })?;
    let evidence_limited_changes = u32::try_from(
        records
            .iter()
            .filter(|record| !record.authoritative)
            .count(),
    )
    .map_err(|_| DocsightError::ResourceLimit {
        resource: "evidence-limited semantic changes".to_owned(),
        limit: u64::from(u32::MAX),
    })?;

    Ok(SemanticDiff {
        total_changes,
        headings,
        paragraphs,
        tables,
        images,
        records,
        lineage,
        lineage_ambiguous,
        evidence_limited_changes,
    })
}

fn align_item(
    document: &Document,
    neighborhoods: &BTreeMap<String, Neighborhood>,
    block: &Block,
    key: String,
    display: String,
    metadata: AlignMetadata,
) -> AlignItem {
    AlignItem {
        document_sha256: document.sha256.clone(),
        object_id: block.id.to_string(),
        page: block.page,
        bbox: block.bbox,
        source_path: block.source.path.clone(),
        style_id: metadata.style_id,
        table_shape: metadata.table_shape,
        image_digest: metadata.image_digest,
        confidence: block.confidence,
        neighborhood: neighborhoods
            .get(block.id.as_str())
            .cloned()
            .unwrap_or_default(),
        key,
        display,
    }
}

fn document_neighborhoods(document: &Document) -> BTreeMap<String, Neighborhood> {
    let text: Vec<Option<String>> = document
        .blocks
        .iter()
        .map(|block| {
            let normalized = normalize_text(&block.text());
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        })
        .collect();
    let mut previous = vec![None; document.blocks.len()];
    let mut last = None;
    for (index, value) in text.iter().enumerate() {
        previous[index] = last.clone();
        if value.is_some() {
            last = value.clone();
        }
    }
    let mut next = vec![None; document.blocks.len()];
    let mut following = None;
    for (index, value) in text.iter().enumerate().rev() {
        next[index] = following.clone();
        if value.is_some() {
            following = value.clone();
        }
    }
    document
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            (
                block.id.to_string(),
                Neighborhood {
                    previous: previous[index].clone(),
                    next: next[index].clone(),
                },
            )
        })
        .collect()
}

fn diff_evidence(
    before_item: Option<&AlignItem>,
    after_item: Option<&AlignItem>,
    context: &DiffContext<'_>,
) -> Vec<DiffEvidence> {
    let mut evidence = Vec::new();
    append_diagnostic_evidence(
        &mut evidence,
        DiffSide::Before,
        before_item,
        context.before_warnings,
    );
    append_diagnostic_evidence(
        &mut evidence,
        DiffSide::After,
        after_item,
        context.after_warnings,
    );
    append_confidence_evidence(
        &mut evidence,
        DiffSide::Before,
        before_item,
        context.target_type,
    );
    append_confidence_evidence(
        &mut evidence,
        DiffSide::After,
        after_item,
        context.target_type,
    );
    evidence.sort_by(|left, right| {
        left.side
            .cmp(&right.side)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.object.cmp(&right.object))
            .then_with(|| left.page.cmp(&right.page))
            .then_with(|| left.message.cmp(&right.message))
    });
    evidence.dedup();
    evidence
}

fn append_diagnostic_evidence(
    evidence: &mut Vec<DiffEvidence>,
    side: DiffSide,
    item: Option<&AlignItem>,
    warnings: &[Diagnostic],
) {
    for warning in warnings {
        if !warning_applies(warning, item) {
            continue;
        }
        evidence.push(DiffEvidence {
            side,
            code: warning.code.clone(),
            message: warning.message.clone(),
            effect: warning.effect.clone(),
            object: warning.object.as_ref().map(ToString::to_string),
            page: warning.page,
        });
    }
}

fn warning_applies(warning: &Diagnostic, item: Option<&AlignItem>) -> bool {
    match item {
        Some(item) => match (&warning.object, warning.page) {
            (Some(object), _) => object.as_str() == item.object_id,
            (None, Some(page)) => item.page == Some(page),
            (None, None) => true,
        },
        None => warning.object.is_none() && warning.page.is_none(),
    }
}

fn append_confidence_evidence(
    evidence: &mut Vec<DiffEvidence>,
    side: DiffSide,
    item: Option<&AlignItem>,
    target_type: &str,
) {
    let Some(item) = item else {
        return;
    };
    if item.confidence >= 0.999 {
        return;
    }
    evidence.push(DiffEvidence {
        side,
        code: "LOW_CONFIDENCE_RECONSTRUCTION".to_owned(),
        message: format!(
            "{target_type} {} has reconstruction confidence {:.3}",
            item.object_id, item.confidence
        ),
        effect: "the semantic change is not authoritative without corroborating source or visual evidence"
            .to_owned(),
        object: Some(item.object_id.clone()),
        page: item.page,
    });
}

fn calculate_largest_drift(before: &Document, after: &Document) -> (Option<f32>, Option<u32>) {
    let mut max_drift = 0.0_f32;
    let mut drift_page = None;

    let b_blocks: Vec<_> = before
        .blocks
        .iter()
        .filter_map(|b| {
            let t = b.text();
            if t.trim().is_empty() {
                None
            } else {
                Some((t, b.page, b.bbox))
            }
        })
        .collect();

    let a_blocks: Vec<_> = after
        .blocks
        .iter()
        .filter_map(|b| {
            let t = b.text();
            if t.trim().is_empty() {
                None
            } else {
                Some((t, b.page, b.bbox))
            }
        })
        .collect();

    for (b_text, b_page, b_bbox) in &b_blocks {
        if let (Some(b_p), Some(b_box)) = (b_page, b_bbox) {
            for (a_text, a_page, a_bbox) in &a_blocks {
                if b_text == a_text
                    && let (Some(a_p), Some(a_box)) = (a_page, a_bbox)
                    && b_p == a_p
                {
                    let drift = (a_box.y0 - b_box.y0).abs();
                    if drift > max_drift {
                        max_drift = drift;
                        drift_page = Some(*b_p);
                    }
                }
            }
        }
    }

    if max_drift > 0.1 {
        (Some(max_drift), drift_page)
    } else {
        (None, None)
    }
}

pub fn diff_visual(
    source_before: &DocumentSource,
    source_after: &DocumentSource,
    doc_before: &Document,
    doc_after: &Document,
    dpi: u16,
    threshold: u8,
    out_dir: Option<&Path>,
) -> Result<VisualDiff, DocsightError> {
    let reason_codes = visual_reason_codes(doc_before, doc_after, None);
    let authoritative = reason_codes.is_empty();
    let evidence_status = if authoritative {
        VisualDiffEvidenceStatus::Exact
    } else {
        VisualDiffEvidenceStatus::EvidenceLimited
    };
    let pages_before =
        u32::try_from(doc_before.pages.len()).map_err(|_| DocsightError::ResourceLimit {
            resource: "visual diff pages".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
    let pages_after =
        u32::try_from(doc_after.pages.len()).map_err(|_| DocsightError::ResourceLimit {
            resource: "visual diff pages".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
    let max_pages = pages_before.max(pages_after);

    let (largest_drift_pt, largest_drift_page) = calculate_largest_drift(doc_before, doc_after);

    let mut layout_changed_pages = 0_u32;
    let mut changed_area_pixels = 0_u64;
    let mut compared_area_pixels = 0_u64;
    let mut page_diffs = Vec::new();

    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|error| DocsightError::Io {
            path: dir.to_path_buf(),
            source: error,
        })?;
    }

    for p in 1..=max_pages {
        let request = RenderRequest {
            target: RenderTarget::Page { page: p },
            dpi,
        };
        let before_image = if p <= pages_before {
            Some(render_document(source_before, &request)?)
        } else {
            None
        };
        let after_image = if p <= pages_after {
            Some(render_document(source_after, &request)?)
        } else {
            None
        };
        let (width, height) = match (&before_image, &after_image) {
            (Some(before), Some(after)) => (
                before.metadata.width_px.max(after.metadata.width_px),
                before.metadata.height_px.max(after.metadata.height_px),
            ),
            (Some(before), None) => (before.metadata.width_px, before.metadata.height_px),
            (None, Some(after)) => (after.metadata.width_px, after.metadata.height_px),
            (None, None) => {
                return Err(DocsightError::MalformedDocument {
                    message: "visual diff page has no source on either side".to_owned(),
                });
            }
        };
        let total_pixels =
            width
                .checked_mul(height)
                .ok_or_else(|| DocsightError::ResourceLimit {
                    resource: "visual diff pixels".to_owned(),
                    limit: u64::from(u32::MAX),
                })?;
        let canvas_bytes = usize::try_from(total_pixels)
            .ok()
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "visual diff raster bytes".to_owned(),
                limit: u64::from(u32::MAX) * 3,
            })?;
        let mut diff_canvas = vec![245_u8; canvas_bytes];
        let scale = 72.0 / f32::from(dpi);
        let (changed_pixels, changed_regions) = match (&before_image, &after_image) {
            (Some(before), Some(after)) => compare_visual_pages(
                before,
                after,
                width,
                height,
                threshold,
                scale,
                &mut diff_canvas,
            )?,
            (Some(before), None) => {
                fill_changed_canvas(&mut diff_canvas);
                (total_pixels, vec![before.metadata.bbox])
            }
            (None, Some(after)) => {
                fill_changed_canvas(&mut diff_canvas);
                (total_pixels, vec![after.metadata.bbox])
            }
            (None, None) => {
                return Err(DocsightError::MalformedDocument {
                    message: "visual diff page has no source on either side".to_owned(),
                });
            }
        };
        if changed_pixels > 0 {
            layout_changed_pages = layout_changed_pages.checked_add(1).ok_or_else(|| {
                DocsightError::ResourceLimit {
                    resource: "visually changed pages".to_owned(),
                    limit: u64::from(u32::MAX),
                }
            })?;
        }
        let change_fraction = if total_pixels > 0 {
            changed_pixels as f32 / total_pixels as f32
        } else {
            0.0
        };
        changed_area_pixels = changed_area_pixels
            .checked_add(u64::from(changed_pixels))
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "visual diff changed area".to_owned(),
                limit: u64::MAX,
            })?;
        compared_area_pixels = compared_area_pixels
            .checked_add(u64::from(total_pixels))
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "visual diff compared area".to_owned(),
                limit: u64::MAX,
            })?;
        let (diff_png, artifact) = if changed_pixels > 0 || out_dir.is_some() {
            let png_bytes = encode_png(width, height, &diff_canvas)?;
            let artifact = if let Some(dir) = out_dir {
                let relative_path = format!("diff_p{p:04}.png");
                let file_path = dir.join(&relative_path);
                std::fs::write(&file_path, &png_bytes).map_err(|error| DocsightError::Io {
                    path: file_path,
                    source: error,
                })?;
                Some(VisualDiffArtifact {
                    relative_path,
                    media_type: "image/png".to_owned(),
                    bytes: u64::try_from(png_bytes.len()).map_err(|_| {
                        DocsightError::ResourceLimit {
                            resource: "visual diff artifact bytes".to_owned(),
                            limit: u64::MAX,
                        }
                    })?,
                    sha256: sha256_hex(&png_bytes),
                    width_px: width,
                    height_px: height,
                })
            } else {
                None
            };
            (Some(png_bytes), artifact)
        } else {
            (None, None)
        };
        let page_reason_codes = visual_reason_codes(doc_before, doc_after, Some(p));
        let page_authoritative = page_reason_codes.is_empty();
        page_diffs.push(PageVisualDiff {
            page: p,
            authoritative: page_authoritative,
            evidence_status: if page_authoritative {
                VisualDiffEvidenceStatus::Exact
            } else {
                VisualDiffEvidenceStatus::EvidenceLimited
            },
            reason_codes: page_reason_codes,
            changed_pixels,
            total_pixels,
            change_fraction,
            changed_regions,
            artifact,
            diff_png,
        });
    }

    let layout_regression_score = if compared_area_pixels > 0 {
        (changed_area_pixels as f64 / compared_area_pixels as f64) as f32
    } else {
        0.0
    };

    Ok(VisualDiff {
        authoritative,
        evidence_status,
        reason_codes,
        dpi,
        threshold,
        pages_before,
        pages_after,
        layout_changed_pages,
        layout_regression_score,
        largest_drift_pt,
        largest_drift_page,
        page_diffs,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn visual_reason_codes(before: &Document, after: &Document, page: Option<u32>) -> Vec<String> {
    before
        .warnings
        .iter()
        .chain(after.warnings.iter())
        .filter(|warning| page.is_none() || warning.page.is_none() || warning.page == page)
        .map(|warning| warning.code.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn compare_visual_pages(
    before: &docsight_render::RenderedImage,
    after: &docsight_render::RenderedImage,
    width: u32,
    height: u32,
    threshold: u8,
    scale: f32,
    diff_canvas: &mut [u8],
) -> Result<(u32, Vec<Rect>), DocsightError> {
    let mut changed_pixels = 0_u32;
    let mut min_x_pt = f32::INFINITY;
    let mut min_y_pt = f32::INFINITY;
    let mut max_x_pt = f32::NEG_INFINITY;
    let mut max_y_pt = f32::NEG_INFINITY;
    for y in 0..height {
        for x in 0..width {
            let before_rgb = visual_pixel(before, x, y)?;
            let after_rgb = visual_pixel(after, x, y)?;
            let difference = before_rgb
                .0
                .abs_diff(after_rgb.0)
                .max(before_rgb.1.abs_diff(after_rgb.1))
                .max(before_rgb.2.abs_diff(after_rgb.2));
            let output_index = rgb_index(x, y, width)?;
            if difference > threshold {
                changed_pixels =
                    changed_pixels
                        .checked_add(1)
                        .ok_or_else(|| DocsightError::ResourceLimit {
                            resource: "changed visual diff pixels".to_owned(),
                            limit: u64::from(u32::MAX),
                        })?;
                diff_canvas[output_index] = 235;
                diff_canvas[output_index + 1] = 40;
                diff_canvas[output_index + 2] = 40;
                let x_pt = x as f32 * scale;
                let y_pt = y as f32 * scale;
                min_x_pt = min_x_pt.min(x_pt);
                min_y_pt = min_y_pt.min(y_pt);
                max_x_pt = max_x_pt.max(x_pt);
                max_y_pt = max_y_pt.max(y_pt);
            } else {
                let gray =
                    ((u16::from(before_rgb.0) + u16::from(before_rgb.1) + u16::from(before_rgb.2))
                        / 3) as u8;
                let subdued = 220 + (gray / 8);
                diff_canvas[output_index] = subdued;
                diff_canvas[output_index + 1] = subdued;
                diff_canvas[output_index + 2] = subdued;
            }
        }
    }
    let regions = if changed_pixels == 0 {
        Vec::new()
    } else {
        vec![Rect::new(
            min_x_pt,
            min_y_pt,
            max_x_pt.max(min_x_pt + 1.0),
            max_y_pt.max(min_y_pt + 1.0),
        )?]
    };
    Ok((changed_pixels, regions))
}

fn visual_pixel(
    image: &docsight_render::RenderedImage,
    x: u32,
    y: u32,
) -> Result<(u8, u8, u8), DocsightError> {
    if x >= image.metadata.width_px || y >= image.metadata.height_px {
        return Ok((255, 255, 255));
    }
    let index = rgb_index(x, y, image.metadata.width_px)?;
    let pixels = image.pixels();
    Ok((pixels[index], pixels[index + 1], pixels[index + 2]))
}

fn fill_changed_canvas(canvas: &mut [u8]) {
    for pixel in canvas.chunks_exact_mut(3) {
        pixel[0] = 235;
        pixel[1] = 40;
        pixel[2] = 40;
    }
}

fn rgb_index(x: u32, y: u32, width: u32) -> Result<usize, DocsightError> {
    let pixel = y
        .checked_mul(width)
        .and_then(|row| row.checked_add(x))
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "visual diff pixel index".to_owned(),
            limit: u64::from(u32::MAX),
        })?;
    usize::try_from(pixel)
        .ok()
        .and_then(|index| index.checked_mul(3))
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "visual diff raster index".to_owned(),
            limit: u64::from(u32::MAX) * 3,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_counter_totals_and_formats() {
        let counter = ChangeCounter {
            added: 1,
            removed: 0,
            modified: 2,
            moved: 0,
        };
        assert_eq!(counter.total(), 3);
        assert_eq!(counter.format_images(), "+1 / -0 / changed 2 / moved 0");
        assert_eq!(counter.format_tables(), "+1 / -0 / modified 2 / moved 0");

        let mod_only = ChangeCounter {
            added: 0,
            removed: 0,
            modified: 2,
            moved: 0,
        };
        assert_eq!(mod_only.format_tables(), "2 modified / 0 moved");
    }

    #[test]
    fn diff_summary_formats_like_specification() {
        let summary = DiffSummary {
            pages_before: 12,
            pages_after: 13,
            semantic_changes: 18,
            layout_changed_pages: 7,
            largest_drift_pt: Some(41.2),
            largest_drift_page: Some(6),
            images: ChangeCounter {
                added: 1,
                removed: 0,
                modified: 0,
                moved: 0,
            },
            tables: ChangeCounter {
                added: 0,
                removed: 0,
                modified: 2,
                moved: 0,
            },
            lineage_ambiguous: 2,
            evidence_limited_changes: 3,
            warnings: vec!["footer on page 13 overlaps body by 6.4 pt".into()],
        };

        let formatted = summary.format_summary();
        assert!(formatted.contains("Pages              12 → 13"));
        assert!(formatted.contains("Semantic changes   18"));
        assert!(formatted.contains("Layout changes     7 pages"));
        assert!(formatted.contains("Largest drift      page 6, 41.2 pt vertical"));
        assert!(formatted.contains("Images             +1 / -0 / changed 0"));
        assert!(formatted.contains("Tables             2 modified / 0 moved"));
        assert!(formatted.contains("Lineage ambiguous"));
        assert!(formatted.contains("Evidence-limited"));
        assert!(formatted.contains("Warnings           footer on page 13 overlaps body by 6.4 pt"));
    }
}

#[cfg(test)]
mod m13_lineage_tests {
    use super::{
        AlignItem, ChangeKind, LineageStatus, Neighborhood, align_items, ambiguous_lineage,
        moved_exact_pairs, text_similarity,
    };

    fn item(key: &str) -> AlignItem {
        item_with(&format!("obj_{key}"), &format!("source/{key}"), key)
    }

    fn item_with(object_id: &str, source_path: &str, key: &str) -> AlignItem {
        AlignItem {
            document_sha256: "a".repeat(64),
            object_id: object_id.to_owned(),
            page: Some(1),
            bbox: None,
            source_path: source_path.to_owned(),
            style_id: Some("Normal".to_owned()),
            table_shape: None,
            image_digest: None,
            confidence: 1.0,
            neighborhood: Neighborhood::default(),
            key: key.to_owned(),
            display: key.to_owned(),
        }
    }

    #[test]
    fn single_insertion_does_not_cascade_into_modifications() {
        let before = vec![item("alpha"), item("beta"), item("gamma")];
        let after = vec![
            item("alpha"),
            item("new entry"),
            item("beta"),
            item("gamma"),
        ];
        let alignment = align_items(&before, &after);
        let stable = moved_exact_pairs(&alignment.pairs);
        assert!(alignment.ambiguities.is_empty());
        assert_eq!(alignment.pairs.len(), 3);
        let moved = alignment
            .pairs
            .iter()
            .enumerate()
            .filter(|(position, _)| !stable.contains(position))
            .count();
        assert_eq!(
            moved, 0,
            "exact matches must stay stable across an insertion"
        );
        assert!(text_similarity("beta", "new entry") < 0.6);
    }

    #[test]
    fn reordered_exact_matches_are_detected_as_moved() {
        let before = vec![item("alpha"), item("beta"), item("gamma")];
        let after = vec![item("gamma"), item("alpha"), item("beta")];
        let alignment = align_items(&before, &after);
        let stable = moved_exact_pairs(&alignment.pairs);
        assert!(alignment.ambiguities.is_empty());
        assert_eq!(alignment.pairs.len(), 3);
        assert!(
            stable.len() < 3,
            "at least one pair must be flagged as moved"
        );
    }

    #[test]
    fn similarity_matches_modified_pairs_with_score() {
        let before = vec![item("revenue increased by twenty percent")];
        let after = vec![item("revenue increased by twenty five percent")];
        let alignment = align_items(&before, &after);
        assert!(alignment.ambiguities.is_empty());
        assert_eq!(alignment.pairs.len(), 1);
        assert!(!alignment.pairs[0].exact);
        assert!(alignment.pairs[0].score >= 0.6);
        assert!(alignment.pairs[0].score < 1.0);
    }

    #[test]
    fn identical_candidates_are_exposed_as_ambiguous_lineage() {
        let before = vec![
            item_with("before_a", "source/shared", "duplicate paragraph"),
            item_with("before_b", "source/shared", "duplicate paragraph"),
        ];
        let after = vec![
            item_with("after_a", "source/shared", "duplicate paragraph"),
            item_with("after_b", "source/shared", "duplicate paragraph"),
        ];
        let alignment = align_items(&before, &after);
        assert!(alignment.pairs.is_empty());
        assert_eq!(alignment.ambiguities.len(), 2);
        let record = ambiguous_lineage("paragraph", &alignment.ambiguities[0], &before, &after);
        assert_eq!(record.status, LineageStatus::Ambiguous);
        assert_eq!(record.candidates.len(), 2);
        assert!(record.before_object.is_some());
        assert!(record.after_object.is_none());
    }

    #[test]
    fn change_kind_covers_moved() {
        assert_eq!(ChangeKind::Moved.to_string(), "moved");
    }
}
