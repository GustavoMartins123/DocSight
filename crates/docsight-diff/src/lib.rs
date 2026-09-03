use docsight_core::{Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, Rect};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_render::{RenderRequest, RenderTarget, encode_png, render_document};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
}

impl ChangeCounter {
    pub fn total(&self) -> u32 {
        self.added
            .saturating_add(self.removed)
            .saturating_add(self.modified)
    }

    pub fn format_images(&self) -> String {
        format!(
            "+{} / -{} / changed {}",
            self.added, self.removed, self.modified
        )
    }

    pub fn format_tables(&self) -> String {
        if self.added > 0 || self.removed > 0 {
            format!(
                "+{} / -{} / modified {}",
                self.added, self.removed, self.modified
            )
        } else {
            format!("{} modified", self.modified)
        }
    }
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

pub fn diff_semantic(before: &Document, after: &Document) -> Result<SemanticDiff, DocsightError> {
    let mut records = Vec::new();
    let mut headings = ChangeCounter::default();
    let mut paragraphs = ChangeCounter::default();
    let mut tables = ChangeCounter::default();
    let mut images = ChangeCounter::default();

    let h_before: Vec<_> = before.headings().collect();
    let h_after: Vec<_> = after.headings().collect();

    let max_h = h_before.len().max(h_after.len());
    for i in 0..max_h {
        match (h_before.get(i), h_after.get(i)) {
            (Some((_b_block, b_h)), Some((a_block, a_h))) => {
                if b_h.text != a_h.text || b_h.level != a_h.level {
                    headings.modified += 1;
                    records.push(SemanticChangeRecord {
                        kind: ChangeKind::Modified,
                        object_id: Some(a_block.id.to_string()),
                        page: a_block.page,
                        target_type: "heading".into(),
                        description: format!(
                            "Heading modified: \"{}\" -> \"{}\"",
                            b_h.text, a_h.text
                        ),
                        before: Some(b_h.text.clone()),
                        after: Some(a_h.text.clone()),
                    });
                }
            }
            (None, Some((a_block, a_h))) => {
                headings.added += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Added,
                    object_id: Some(a_block.id.to_string()),
                    page: a_block.page,
                    target_type: "heading".into(),
                    description: format!("Heading added: \"{}\"", a_h.text),
                    before: None,
                    after: Some(a_h.text.clone()),
                });
            }
            (Some((b_block, b_h)), None) => {
                headings.removed += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Removed,
                    object_id: Some(b_block.id.to_string()),
                    page: b_block.page,
                    target_type: "heading".into(),
                    description: format!("Heading removed: \"{}\"", b_h.text),
                    before: Some(b_h.text.clone()),
                    after: None,
                });
            }
            (None, None) => {}
        }
    }

    let p_before: Vec<_> = before.paragraphs().collect();
    let p_after: Vec<_> = after.paragraphs().collect();

    let max_p = p_before.len().max(p_after.len());
    for i in 0..max_p {
        match (p_before.get(i), p_after.get(i)) {
            (Some((_b_block, b_p)), Some((a_block, a_p))) => {
                if b_p.text != a_p.text {
                    paragraphs.modified += 1;
                    records.push(SemanticChangeRecord {
                        kind: ChangeKind::Modified,
                        object_id: Some(a_block.id.to_string()),
                        page: a_block.page,
                        target_type: "paragraph".into(),
                        description: "Paragraph text modified".into(),
                        before: Some(b_p.text.clone()),
                        after: Some(a_p.text.clone()),
                    });
                }
            }
            (None, Some((a_block, a_p))) => {
                paragraphs.added += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Added,
                    object_id: Some(a_block.id.to_string()),
                    page: a_block.page,
                    target_type: "paragraph".into(),
                    description: "Paragraph added".into(),
                    before: None,
                    after: Some(a_p.text.clone()),
                });
            }
            (Some((b_block, b_p)), None) => {
                paragraphs.removed += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Removed,
                    object_id: Some(b_block.id.to_string()),
                    page: b_block.page,
                    target_type: "paragraph".into(),
                    description: "Paragraph removed".into(),
                    before: Some(b_p.text.clone()),
                    after: None,
                });
            }
            (None, None) => {}
        }
    }

    let t_before: Vec<_> = before.tables().collect();
    let t_after: Vec<_> = after.tables().collect();

    let max_t = t_before.len().max(t_after.len());
    for i in 0..max_t {
        match (t_before.get(i), t_after.get(i)) {
            (Some((_b_block, b_t)), Some((a_block, a_t))) => {
                let diff_dims = b_t.rows != a_t.rows || b_t.columns != a_t.columns;
                let diff_cells = b_t.cells.len() != a_t.cells.len()
                    || b_t
                        .cells
                        .iter()
                        .zip(&a_t.cells)
                        .any(|(bc, ac)| bc.text != ac.text);
                if diff_dims || diff_cells {
                    tables.modified += 1;
                    records.push(SemanticChangeRecord {
                        kind: ChangeKind::Modified,
                        object_id: Some(a_block.id.to_string()),
                        page: a_block.page,
                        target_type: "table".into(),
                        description: format!(
                            "Table modified ({}x{} -> {}x{})",
                            b_t.rows, b_t.columns, a_t.rows, a_t.columns
                        ),
                        before: Some(format!("{}x{}", b_t.rows, b_t.columns)),
                        after: Some(format!("{}x{}", a_t.rows, a_t.columns)),
                    });
                }
            }
            (None, Some((a_block, a_t))) => {
                tables.added += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Added,
                    object_id: Some(a_block.id.to_string()),
                    page: a_block.page,
                    target_type: "table".into(),
                    description: format!("Table added ({}x{})", a_t.rows, a_t.columns),
                    before: None,
                    after: Some(format!("{}x{}", a_t.rows, a_t.columns)),
                });
            }
            (Some((b_block, b_t)), None) => {
                tables.removed += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Removed,
                    object_id: Some(b_block.id.to_string()),
                    page: b_block.page,
                    target_type: "table".into(),
                    description: format!("Table removed ({}x{})", b_t.rows, b_t.columns),
                    before: Some(format!("{}x{}", b_t.rows, b_t.columns)),
                    after: None,
                });
            }
            (None, None) => {}
        }
    }

    let f_before: Vec<_> = before.figures().collect();
    let f_after: Vec<_> = after.figures().collect();
    let max_f = f_before.len().max(f_after.len());
    for i in 0..max_f {
        match (f_before.get(i), f_after.get(i)) {
            (Some((_b_block, b_f)), Some((a_block, a_f))) => {
                if b_f.caption != a_f.caption || b_f.alt_text != a_f.alt_text {
                    images.modified += 1;
                    records.push(SemanticChangeRecord {
                        kind: ChangeKind::Modified,
                        object_id: Some(a_block.id.to_string()),
                        page: a_block.page,
                        target_type: "figure".into(),
                        description: "Figure properties modified".into(),
                        before: b_f.caption.clone(),
                        after: a_f.caption.clone(),
                    });
                }
            }
            (None, Some((a_block, a_f))) => {
                images.added += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Added,
                    object_id: Some(a_block.id.to_string()),
                    page: a_block.page,
                    target_type: "figure".into(),
                    description: "Figure added".into(),
                    before: None,
                    after: a_f.caption.clone(),
                });
            }
            (Some((b_block, b_f)), None) => {
                images.removed += 1;
                records.push(SemanticChangeRecord {
                    kind: ChangeKind::Removed,
                    object_id: Some(b_block.id.to_string()),
                    page: b_block.page,
                    target_type: "figure".into(),
                    description: "Figure removed".into(),
                    before: b_f.caption.clone(),
                    after: None,
                });
            }
            (None, None) => {}
        }
    }

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
        };
        assert_eq!(counter.total(), 3);
        assert_eq!(counter.format_images(), "+1 / -0 / changed 2");
        assert_eq!(counter.format_tables(), "+1 / -0 / modified 2");

        let mod_only = ChangeCounter {
            added: 0,
            removed: 0,
            modified: 2,
        };
        assert_eq!(mod_only.format_tables(), "2 modified");
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
            },
            tables: ChangeCounter {
                added: 0,
                removed: 0,
                modified: 2,
            },
            warnings: vec!["footer on page 13 overlaps body by 6.4 pt".into()],
        };

        let formatted = summary.format_summary();
        assert!(formatted.contains("Pages              12 → 13"));
        assert!(formatted.contains("Semantic changes   18"));
        assert!(formatted.contains("Layout changes     7 pages"));
        assert!(formatted.contains("Largest drift      page 6, 41.2 pt vertical"));
        assert!(formatted.contains("Images             +1 / -0 / changed 0"));
        assert!(formatted.contains("Tables             2 modified"));
        assert!(formatted.contains("Warnings           footer on page 13 overlaps body by 6.4 pt"));
    }
}
