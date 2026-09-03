use crate::{
    BlockKind, Diagnostic, DocsightError, Document, DocumentFormat, DocumentSource, ObjectId, Rect,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FidelityProfile {
    pub text: f32,
    pub structure: f32,
    pub geometry: f32,
    pub visual: f32,
    pub reasons: Vec<String>,
}

impl FidelityProfile {
    pub fn for_docx(warnings: &[Diagnostic]) -> Self {
        let mut reasons: Vec<String> = warnings.iter().map(|w| w.code.clone()).collect();
        reasons.sort();
        reasons.dedup();
        let has_font_sub = reasons.iter().any(|r| r == "DOCX_FONT_SUBSTITUTED");
        let has_layout_pag = reasons.iter().any(|r| r == "DOCX_LAYOUT_PAGINATED");
        let geometry = if has_font_sub {
            0.943
        } else if has_layout_pag {
            0.980
        } else {
            1.000
        };
        let visual = if has_font_sub { 0.812 } else { 0.980 };
        Self {
            text: 1.000,
            structure: 1.000,
            geometry,
            visual,
            reasons,
        }
    }

    pub fn for_pdf(warnings: &[Diagnostic], block_confidence: Option<f32>) -> Self {
        let mut reasons: Vec<String> = warnings.iter().map(|w| w.code.clone()).collect();
        reasons.sort();
        reasons.dedup();
        let structure = block_confidence.unwrap_or(0.850);
        let has_font_approx = reasons
            .iter()
            .any(|r| r.contains("FONT") || r.contains("BASE14"));
        let visual = if has_font_approx { 0.880 } else { 0.950 };
        Self {
            text: 1.000,
            structure,
            geometry: 0.990,
            visual,
            reasons,
        }
    }

    pub fn overall(&self) -> f32 {
        (self.text + self.structure + self.geometry + self.visual) / 4.0
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
    pub safe_source_fragment: String,
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
    pub reason_codes: Vec<String>,
}

pub fn compute_evidence(
    doc: &Document,
    source: &DocumentSource,
    object_id: &ObjectId,
    render_fingerprint: Option<String>,
) -> Result<EvidenceRecord, DocsightError> {
    let block =
        doc.find_block(object_id.as_str())
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: object_id.to_string(),
            })?;

    let fidelity = match source.format() {
        DocumentFormat::Docx => FidelityProfile::for_docx(&doc.warnings),
        DocumentFormat::Pdf => FidelityProfile::for_pdf(&doc.warnings, Some(block.confidence)),
    };

    let full_text = block.text();
    let safe_source_fragment = if full_text.len() > 300 {
        format!("{}...", &full_text[..300])
    } else {
        full_text
    };

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
        safe_source_fragment,
    })
}

pub fn compute_coverage(
    doc: &Document,
    source: &DocumentSource,
    page_filter: Option<u32>,
    include_regions: bool,
) -> Result<CoverageReport, DocsightError> {
    let mut page_coverages = Vec::new();
    let mut all_affected_ids = std::collections::BTreeSet::new();
    let mut all_reason_codes = std::collections::BTreeSet::new();

    let doc_fidelity = match source.format() {
        DocumentFormat::Docx => FidelityProfile::for_docx(&doc.warnings),
        DocumentFormat::Pdf => FidelityProfile::for_pdf(&doc.warnings, None),
    };

    for code in &doc_fidelity.reasons {
        all_reason_codes.insert(code.clone());
    }

    let pages_to_process: Vec<u32> = match page_filter {
        Some(p) => vec![p],
        None => doc.pages.iter().map(|page| page.number).collect(),
    };

    for page_num in pages_to_process {
        let page_blocks: Vec<_> = doc
            .blocks
            .iter()
            .filter(|b| b.page == Some(page_num))
            .collect();

        let mut regions = Vec::new();

        if include_regions {
            for block in &page_blocks {
                match source.format() {
                    DocumentFormat::Docx => {
                        if doc_fidelity
                            .reasons
                            .iter()
                            .any(|r| r == "DOCX_FONT_SUBSTITUTED")
                        {
                            all_affected_ids.insert(block.id.to_string());
                            regions.push(CoverageRegion {
                                page: page_num,
                                object_id: Some(block.id.clone()),
                                bbox: block.bbox,
                                status: CoverageStatus::Approximated,
                                reason_code: "DOCX_FONT_SUBSTITUTED".to_owned(),
                                description: "proportional fallback metrics used for run layout"
                                    .to_owned(),
                            });
                        }
                    }
                    DocumentFormat::Pdf => {
                        if block.confidence < 0.95 {
                            all_affected_ids.insert(block.id.to_string());
                            let reason = if block.kind == BlockKind::Table {
                                "INFERRED_TABLE"
                            } else {
                                "INFERRED_SEMANTICS"
                            };
                            all_reason_codes.insert(reason.to_owned());
                            regions.push(CoverageRegion {
                                page: page_num,
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
            }
        } else {
            for block in &page_blocks {
                if block.confidence < 0.95 || source.format() == DocumentFormat::Docx {
                    all_affected_ids.insert(block.id.to_string());
                }
            }
        }

        let geom_status = if doc_fidelity.geometry < 0.999 {
            CoverageStatus::Approximated
        } else {
            CoverageStatus::Exact
        };

        let visual_status = if doc_fidelity.visual < 0.999 {
            CoverageStatus::Approximated
        } else {
            CoverageStatus::Exact
        };

        let struct_status = if doc_fidelity.structure < 0.999 {
            CoverageStatus::Inferred
        } else {
            CoverageStatus::Exact
        };

        page_coverages.push(PageCoverage {
            page: page_num,
            text: CoverageMetric {
                score: doc_fidelity.text,
                status: CoverageStatus::Exact,
                reason_codes: Vec::new(),
            },
            structure: CoverageMetric {
                score: doc_fidelity.structure,
                status: struct_status,
                reason_codes: doc_fidelity.reasons.clone(),
            },
            geometry: CoverageMetric {
                score: doc_fidelity.geometry,
                status: geom_status,
                reason_codes: doc_fidelity.reasons.clone(),
            },
            visual: CoverageMetric {
                score: doc_fidelity.visual,
                status: visual_status,
                reason_codes: doc_fidelity.reasons.clone(),
            },
            overall_fidelity: doc_fidelity.overall(),
            regions,
        });
    }

    let global = if let Some(first) = page_coverages.first() {
        first.clone()
    } else {
        PageCoverage {
            page: 1,
            text: CoverageMetric {
                score: 1.0,
                status: CoverageStatus::Exact,
                reason_codes: Vec::new(),
            },
            structure: CoverageMetric {
                score: 1.0,
                status: CoverageStatus::Exact,
                reason_codes: Vec::new(),
            },
            geometry: CoverageMetric {
                score: 1.0,
                status: CoverageStatus::Exact,
                reason_codes: Vec::new(),
            },
            visual: CoverageMetric {
                score: 1.0,
                status: CoverageStatus::Exact,
                reason_codes: Vec::new(),
            },
            overall_fidelity: 1.0,
            regions: Vec::new(),
        }
    };

    let reason_codes: Vec<String> = all_reason_codes.into_iter().collect();

    Ok(CoverageReport {
        document_sha256: source.sha256().to_owned(),
        format: source.format(),
        global,
        pages: page_coverages,
        affected_objects_count: all_affected_ids.len(),
        reason_codes,
    })
}
