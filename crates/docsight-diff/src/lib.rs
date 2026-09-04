use docsight_core::{
    Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, Rect, table_to_tsv_string,
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
    #[serde(default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_object: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_object: Option<String>,
    pub match_score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChangeKind>,
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
    #[serde(default)]
    pub lineage: Vec<LineageRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageVisualDiff {
    pub page: u32,
    pub changed_pixels: u32,
    pub total_pixels: u32,
    pub change_fraction: f32,
    pub changed_regions: Vec<Rect>,
    #[serde(skip_serializing)]
    pub diff_png: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VisualDiff {
    pub pages_before: u32,
    pub pages_after: u32,
    pub layout_changed_pages: u32,
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
        for w in &self.warnings {
            lines.push(format!("{:<18} {}", "Warnings", w));
        }
        lines.join("\n")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentDiffResult {
    pub format_before: DocumentFormat,
    pub format_after: DocumentFormat,
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
        warnings: summary_warnings,
    };

    Ok(DocumentDiffResult {
        format_before: source_before.format(),
        format_after: source_after.format(),
        package,
        semantic,
        visual,
        summary,
        warnings: combined_warnings,
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

const SIMILARITY_THRESHOLD: f32 = 0.6;

struct AlignItem {
    object_id: String,
    page: Option<u32>,
    key: String,
    display: String,
}

struct MatchedPair {
    before_index: usize,
    after_index: usize,
    exact: bool,
    score: f32,
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

fn align_items(before: &[AlignItem], after: &[AlignItem]) -> Vec<MatchedPair> {
    let mut pairs = Vec::new();
    let mut after_by_key: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, item) in after.iter().enumerate() {
        after_by_key
            .entry(item.key.as_str())
            .or_default()
            .push(index);
    }
    let mut before_matched = vec![false; before.len()];
    let mut after_matched = vec![false; after.len()];

    for (before_index, item) in before.iter().enumerate() {
        if let Some(candidates) = after_by_key.get(item.key.as_str()) {
            if let Some(&after_index) = candidates.iter().find(|index| !after_matched[**index]) {
                pairs.push(MatchedPair {
                    before_index,
                    after_index,
                    exact: true,
                    score: 1.0,
                });
                before_matched[before_index] = true;
                after_matched[after_index] = true;
            }
        }
    }

    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
    for (before_index, before_item) in before.iter().enumerate() {
        if before_matched[before_index] {
            continue;
        }
        for (after_index, after_item) in after.iter().enumerate() {
            if after_matched[after_index] {
                continue;
            }
            let score = text_similarity(&before_item.key, &after_item.key);
            if score >= SIMILARITY_THRESHOLD {
                candidates.push((score, before_index, after_index));
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    for (score, before_index, after_index) in candidates {
        if before_matched[before_index] || after_matched[after_index] {
            continue;
        }
        pairs.push(MatchedPair {
            before_index,
            after_index,
            exact: false,
            score,
        });
        before_matched[before_index] = true;
        after_matched[after_index] = true;
    }

    pairs.sort_by_key(|pair| pair.after_index);
    pairs
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

fn lineage_id(target_type: &str, before_key: &str, after_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let (first, second) = if before_key <= after_key {
        (before_key, after_key)
    } else {
        (after_key, before_key)
    };
    let mut hasher = Sha256::new();
    hasher.update(target_type.as_bytes());
    hasher.update([0]);
    hasher.update(first.as_bytes());
    hasher.update([0]);
    hasher.update(second.as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("lin_{suffix}")
}

#[allow(clippy::too_many_arguments)]
fn push_change_record(
    records: &mut Vec<SemanticChangeRecord>,
    lineage: &mut Vec<LineageRecord>,
    counter: &mut ChangeCounter,
    target_type: &str,
    kind: ChangeKind,
    before_item: Option<&AlignItem>,
    after_item: Option<&AlignItem>,
    score: Option<f32>,
) {
    let record_lineage = match (before_item, after_item) {
        (Some(before), Some(after)) => {
            let record = LineageRecord {
                id: lineage_id(target_type, &before.key, &after.key),
                before_object: Some(before.object_id.clone()),
                after_object: Some(after.object_id.clone()),
                match_score: score.unwrap_or(1.0),
                kind: Some(kind),
            };
            lineage.push(LineageRecord {
                kind: None,
                ..record.clone()
            });
            Some(record)
        }
        _ => None,
    };
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
                score = score.unwrap_or(1.0)
            ),
            Some(before.display.clone()),
            Some(after.display.clone()),
        ),
        (Some(before), Some(after)) => (
            format!(
                "{target_type} moved: was position-linked to \"{}\"",
                before.display
            ),
            Some(before.display.clone()),
            Some(after.display.clone()),
        ),
        (None, Some(after)) => (
            format!("{target_type} added: \"{}\"", after.display),
            None,
            Some(after.display.clone()),
        ),
        (Some(before), None) => (
            format!("{target_type} removed: \"{}\"", before.display),
            Some(before.display.clone()),
            None,
        ),
        (None, None) => (format!("{target_type} changed"), None, None),
    };
    match kind {
        ChangeKind::Added => counter.added += 1,
        ChangeKind::Removed => counter.removed += 1,
        ChangeKind::Modified => counter.modified += 1,
        ChangeKind::Moved => counter.moved += 1,
    }
    records.push(SemanticChangeRecord {
        kind,
        object_id,
        page,
        target_type: target_type.to_owned(),
        description,
        before: before_text,
        after: after_text,
        lineage: record_lineage,
    });
}

fn diff_collection(
    before: &[AlignItem],
    after: &[AlignItem],
    target_type: &str,
    records: &mut Vec<SemanticChangeRecord>,
    lineage: &mut Vec<LineageRecord>,
) -> ChangeCounter {
    let mut counter = ChangeCounter::default();
    let pairs = align_items(before, after);
    let stable_positions = moved_exact_pairs(&pairs);

    let mut handled_before = vec![false; before.len()];
    let mut handled_after = vec![false; after.len()];

    for (position, pair) in pairs.iter().enumerate() {
        handled_before[pair.before_index] = true;
        handled_after[pair.after_index] = true;
        if pair.exact && stable_positions.contains(&position) {
            lineage.push(LineageRecord {
                id: lineage_id(
                    target_type,
                    &before[pair.before_index].key,
                    &after[pair.after_index].key,
                ),
                before_object: Some(before[pair.before_index].object_id.clone()),
                after_object: Some(after[pair.after_index].object_id.clone()),
                match_score: 1.0,
                kind: None,
            });
            continue;
        }
        let kind = if pair.exact {
            ChangeKind::Moved
        } else {
            ChangeKind::Modified
        };
        push_change_record(
            records,
            lineage,
            &mut counter,
            target_type,
            kind,
            Some(&before[pair.before_index]),
            Some(&after[pair.after_index]),
            Some(pair.score),
        );
    }

    for (index, item) in after.iter().enumerate() {
        if !handled_after[index] {
            push_change_record(
                records,
                lineage,
                &mut counter,
                target_type,
                ChangeKind::Added,
                None,
                Some(item),
                None,
            );
        }
    }
    for (index, item) in before.iter().enumerate() {
        if !handled_before[index] {
            push_change_record(
                records,
                lineage,
                &mut counter,
                target_type,
                ChangeKind::Removed,
                Some(item),
                None,
                None,
            );
        }
    }

    counter
}

pub fn diff_semantic(before: &Document, after: &Document) -> Result<SemanticDiff, DocsightError> {
    let mut records = Vec::new();
    let mut lineage = Vec::new();

    let heading_items = |document: &Document| -> Vec<AlignItem> {
        document
            .headings()
            .map(|(block, heading)| AlignItem {
                object_id: block.id.to_string(),
                page: block.page,
                key: normalize_text(&format!("h{}:{}", heading.level, heading.text)),
                display: heading.text.clone(),
            })
            .collect()
    };
    let headings = diff_collection(
        &heading_items(before),
        &heading_items(after),
        "heading",
        &mut records,
        &mut lineage,
    );

    let paragraph_items = |document: &Document| -> Vec<AlignItem> {
        document
            .paragraphs()
            .map(|(block, paragraph)| AlignItem {
                object_id: block.id.to_string(),
                page: block.page,
                key: normalize_text(&paragraph.text),
                display: paragraph.text.clone(),
            })
            .collect()
    };
    let paragraphs = diff_collection(
        &paragraph_items(before),
        &paragraph_items(after),
        "paragraph",
        &mut records,
        &mut lineage,
    );

    let table_items = |document: &Document| -> Vec<AlignItem> {
        document
            .tables()
            .map(|(block, table)| AlignItem {
                object_id: block.id.to_string(),
                page: block.page,
                key: normalize_text(&table_to_tsv_string(table)),
                display: format!("{}x{}", table.rows, table.columns),
            })
            .collect()
    };
    let tables = diff_collection(
        &table_items(before),
        &table_items(after),
        "table",
        &mut records,
        &mut lineage,
    );

    let figure_items = |document: &Document| -> Vec<AlignItem> {
        document
            .figures()
            .map(|(block, figure)| AlignItem {
                object_id: block.id.to_string(),
                page: block.page,
                key: normalize_text(&format!(
                    "{}|{}|{}",
                    figure.resource_id.as_deref().unwrap_or(""),
                    figure.alt_text.as_deref().unwrap_or(""),
                    figure.caption.as_deref().unwrap_or("")
                )),
                display: figure
                    .caption
                    .clone()
                    .or_else(|| figure.alt_text.clone())
                    .unwrap_or_else(|| figure.resource_id.clone().unwrap_or_default()),
            })
            .collect()
    };
    let images = diff_collection(
        &figure_items(before),
        &figure_items(after),
        "figure",
        &mut records,
        &mut lineage,
    );

    records.sort_by(|left, right| {
        left.target_type.cmp(&right.target_type).then_with(|| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.object_id.cmp(&right.object_id))
                .then_with(|| left.description.cmp(&right.description))
        })
    });
    lineage.sort_by(|left, right| left.id.cmp(&right.id));

    let total_changes = headings
        .total()
        .saturating_add(paragraphs.total())
        .saturating_add(tables.total())
        .saturating_add(images.total());

    Ok(SemanticDiff {
        total_changes,
        headings,
        paragraphs,
        tables,
        images,
        records,
        lineage,
    })
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
                if b_text == a_text {
                    if let (Some(a_p), Some(a_box)) = (a_page, a_bbox) {
                        if b_p == a_p {
                            let drift = (a_box.y0 - b_box.y0).abs();
                            if drift > max_drift {
                                max_drift = drift;
                                drift_page = Some(*b_p);
                            }
                        }
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
    let pages_before = doc_before.pages.len() as u32;
    let pages_after = doc_after.pages.len() as u32;
    let max_pages = pages_before.max(pages_after);

    let (largest_drift_pt, largest_drift_page) = calculate_largest_drift(doc_before, doc_after);

    let mut layout_changed_pages = 0_u32;
    let mut page_diffs = Vec::new();

    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|error| DocsightError::Io {
            path: dir.to_path_buf(),
            source: error,
        })?;
    }

    for p in 1..=max_pages {
        if p <= pages_before && p <= pages_after {
            let req = RenderRequest {
                target: RenderTarget::Page { page: p },
                dpi,
            };
            let img_a = render_document(source_before, &req)?;
            let img_b = render_document(source_after, &req)?;

            let w = img_a.metadata.width_px.max(img_b.metadata.width_px);
            let h = img_a.metadata.height_px.max(img_b.metadata.height_px);
            let total_pixels = w.saturating_mul(h);

            let mut diff_canvas = vec![245_u8; (total_pixels as usize) * 3];
            let mut changed_pixels = 0_u32;

            let mut min_x_pt = f32::INFINITY;
            let mut min_y_pt = f32::INFINITY;
            let mut max_x_pt = f32::NEG_INFINITY;
            let mut max_y_pt = f32::NEG_INFINITY;

            let px_a = img_a.pixels();
            let px_b = img_b.pixels();

            let wa = img_a.metadata.width_px;
            let ha = img_a.metadata.height_px;
            let wb = img_b.metadata.width_px;
            let hb = img_b.metadata.height_px;

            let scale = 72.0 / f32::from(dpi);

            for y in 0..h {
                for x in 0..w {
                    let rgb_a = if x < wa && y < ha {
                        let idx = (y * wa + x) as usize * 3;
                        (px_a[idx], px_a[idx + 1], px_a[idx + 2])
                    } else {
                        (255, 255, 255)
                    };

                    let rgb_b = if x < wb && y < hb {
                        let idx = (y * wb + x) as usize * 3;
                        (px_b[idx], px_b[idx + 1], px_b[idx + 2])
                    } else {
                        (255, 255, 255)
                    };

                    let diff_r = rgb_a.0.abs_diff(rgb_b.0);
                    let diff_g = rgb_a.1.abs_diff(rgb_b.1);
                    let diff_b = rgb_a.2.abs_diff(rgb_b.2);
                    let max_diff = diff_r.max(diff_g).max(diff_b);

                    let out_idx = (y * w + x) as usize * 3;
                    if max_diff > threshold {
                        changed_pixels += 1;
                        diff_canvas[out_idx] = 235;
                        diff_canvas[out_idx + 1] = 40;
                        diff_canvas[out_idx + 2] = 40;

                        let x_pt = x as f32 * scale;
                        let y_pt = y as f32 * scale;
                        min_x_pt = min_x_pt.min(x_pt);
                        min_y_pt = min_y_pt.min(y_pt);
                        max_x_pt = max_x_pt.max(x_pt);
                        max_y_pt = max_y_pt.max(y_pt);
                    } else {
                        let gray = ((u16::from(rgb_a.0) + u16::from(rgb_a.1) + u16::from(rgb_a.2))
                            / 3) as u8;
                        let subdued = 220 + (gray / 8);
                        diff_canvas[out_idx] = subdued;
                        diff_canvas[out_idx + 1] = subdued;
                        diff_canvas[out_idx + 2] = subdued;
                    }
                }
            }

            let mut changed_regions = Vec::new();
            if changed_pixels > 0 {
                layout_changed_pages += 1;
                if let Ok(rect) = Rect::new(
                    min_x_pt,
                    min_y_pt,
                    max_x_pt.max(min_x_pt + 1.0),
                    max_y_pt.max(min_y_pt + 1.0),
                ) {
                    changed_regions.push(rect);
                }
            }

            let change_fraction = if total_pixels > 0 {
                changed_pixels as f32 / total_pixels as f32
            } else {
                0.0
            };

            let diff_png = if changed_pixels > 0 || out_dir.is_some() {
                let png_bytes = encode_png(w, h, &diff_canvas)?;
                if let Some(dir) = out_dir {
                    let file_path = dir.join(format!("diff_p{p:04}.png"));
                    std::fs::write(&file_path, &png_bytes).map_err(|error| DocsightError::Io {
                        path: file_path,
                        source: error,
                    })?;
                }
                Some(png_bytes)
            } else {
                None
            };

            page_diffs.push(PageVisualDiff {
                page: p,
                changed_pixels,
                total_pixels,
                change_fraction,
                changed_regions,
                diff_png,
            });
        } else {
            layout_changed_pages += 1;
            page_diffs.push(PageVisualDiff {
                page: p,
                changed_pixels: 0,
                total_pixels: 0,
                change_fraction: 1.0,
                changed_regions: Vec::new(),
                diff_png: None,
            });
        }
    }

    Ok(VisualDiff {
        pages_before,
        pages_after,
        layout_changed_pages,
        largest_drift_pt,
        largest_drift_page,
        page_diffs,
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
            warnings: vec!["footer on page 13 overlaps body by 6.4 pt".into()],
        };

        let formatted = summary.format_summary();
        assert!(formatted.contains("Pages              12 → 13"));
        assert!(formatted.contains("Semantic changes   18"));
        assert!(formatted.contains("Layout changes     7 pages"));
        assert!(formatted.contains("Largest drift      page 6, 41.2 pt vertical"));
        assert!(formatted.contains("Images             +1 / -0 / changed 0"));
        assert!(formatted.contains("Tables             2 modified / 0 moved"));
        assert!(formatted.contains("Warnings           footer on page 13 overlaps body by 6.4 pt"));
    }
}

#[cfg(test)]
mod m8_alignment_tests {
    use super::{AlignItem, ChangeKind, align_items, moved_exact_pairs, text_similarity};

    fn item(key: &str) -> AlignItem {
        AlignItem {
            object_id: format!("obj_{key}"),
            page: Some(1),
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
        let pairs = align_items(&before, &after);
        let stable = moved_exact_pairs(&pairs);
        assert_eq!(pairs.len(), 3);
        let moved = pairs
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
        let pairs = align_items(&before, &after);
        let stable = moved_exact_pairs(&pairs);
        assert_eq!(pairs.len(), 3);
        assert!(
            stable.len() < 3,
            "at least one pair must be flagged as moved"
        );
    }

    #[test]
    fn similarity_matches_modified_pairs_with_score() {
        let before = vec![item("revenue increased by twenty percent")];
        let after = vec![item("revenue increased by twenty five percent")];
        let pairs = align_items(&before, &after);
        assert_eq!(pairs.len(), 1);
        assert!(!pairs[0].exact);
        assert!(pairs[0].score >= 0.6);
        assert!(pairs[0].score < 1.0);
    }

    #[test]
    fn change_kind_covers_moved() {
        assert_eq!(ChangeKind::Moved.to_string(), "moved");
    }
}
