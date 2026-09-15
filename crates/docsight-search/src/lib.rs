use docsight_core::{
    BlockContent, BlockKind, Diagnostic, DocsightError, Document, DocumentFormat, ObjectId,
    OverlayKind, Rect,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

const MAX_DQL_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_OBJECTS: usize = 100_000;
const MAX_SPATIAL_COMPARISONS: usize = 1_000_000;
const MAX_VIEWPORT_PAGES: usize = 32;
const MAX_PEEK_PAGES: usize = 8;
const MAX_RESOLVE_TEXT_BYTES: usize = 4 * 1024;
const RESOLVE_MATCHING: &str = "normalized_lexical";
const RESOLVE_THRESHOLD: f64 = 0.35;
const RESOLVE_AMBIGUITY_MARGIN: f64 = 0.03;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticKind {
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
}

impl From<BlockKind> for SemanticKind {
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

impl From<OverlayKind> for SemanticKind {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticObject {
    pub id: ObjectId,
    pub kind: SemanticKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    pub z_index: i32,
    pub reading_order: u32,
    pub source: String,
    pub confidence: f32,
    pub text_snippet: String,
    pub text_truncated: bool,
}

#[derive(Clone, Debug)]
struct Candidate {
    object: SemanticObject,
    search_text: String,
    heading_level: Option<u8>,
    style_id: Option<String>,
    alt_text: Option<String>,
    note_anchor_path: Option<String>,
}

impl Candidate {
    fn geometry(&self) -> Option<(u32, Rect)> {
        let page = self.object.page?;
        let bbox = self.object.bbox?;
        Rect::new(bbox.x0, bbox.y0, bbox.x1, bbox.y1)
            .ok()
            .map(|rect| (page, rect))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectorKind {
    Object,
    Paragraph,
    Heading,
    ListItem,
    Table,
    Figure,
    Shape,
    Note,
    Unknown,
    Header,
    Footer,
    Watermark,
    CommentMarker,
    Annotation,
    Caption,
}

#[derive(Clone, Debug, PartialEq)]
enum SelectorFilter {
    HeadingLevelAtMost(u8),
    ConfidenceGreaterThan(f32),
    AltEquals(String),
    StyleEquals(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Selector {
    kind: SelectorKind,
    filters: Vec<SelectorFilter>,
    contains: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpatialOperator {
    Above,
    Below,
    Inside,
    Overlaps,
    Nearest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DistanceComparison {
    LessThan,
    LessThanOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
enum QueryExpression {
    Select {
        pages: Option<PageRange>,
        selector: Selector,
    },
    Spatial {
        pages: Option<PageRange>,
        selector: Selector,
        operator: SpatialOperator,
        reference: Selector,
    },
    DistanceTo {
        pages: Option<PageRange>,
        selector: Selector,
        reference_id: String,
        comparison: DistanceComparison,
        distance_pt: f32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageRange {
    pub start: u32,
    pub end: u32,
}

impl PageRange {
    pub fn new(start: u32, end: u32) -> Result<Self, DocsightError> {
        if start == 0 || end == 0 || start > end {
            return Err(DocsightError::InvalidArgument {
                message: "page range must use 1-based inclusive pages with start not after end"
                    .to_owned(),
            });
        }
        Ok(Self { start, end })
    }

    pub fn contains(self, page: u32) -> bool {
        page >= self.start && page <= self.end
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryRelation {
    Above,
    Below,
    Inside,
    Overlaps,
    Nearest,
    DistanceTo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpatialRelation {
    pub kind: QueryRelation,
    pub anchor: ObjectId,
    pub distance_pt: f64,
    pub matching_anchor_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryMatch {
    pub object: SemanticObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<SpatialRelation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpatialQueryResult {
    pub query: String,
    pub total_matches: usize,
    pub geometry_unavailable_objects: usize,
    pub matches: Vec<QueryMatch>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpatialQueryExecution {
    pub result: SpatialQueryResult,
    pub warnings: Vec<Diagnostic>,
}

pub fn execute_spatial_query(
    document: &Document,
    query: &str,
) -> Result<SpatialQueryExecution, DocsightError> {
    let expression = parse_query(query)?;
    if document.format == DocumentFormat::Pdf && expression_uses_caption(&expression) {
        return Err(DocsightError::UnsupportedFeature {
            feature: "DQL caption selector for PDF documents".to_owned(),
        });
    }
    let candidates = candidates(document)?;
    let mut unavailable = BTreeSet::new();
    let mut matches = match expression {
        QueryExpression::Select { pages, selector } => {
            select_candidates(&candidates, &selector, pages)
                .into_iter()
                .map(|candidate| QueryMatch {
                    object: candidate.object.clone(),
                    relation: None,
                })
                .collect()
        }
        QueryExpression::Spatial {
            pages,
            selector,
            operator,
            reference,
        } => execute_relation(
            &candidates,
            pages,
            &selector,
            operator,
            &reference,
            &mut unavailable,
        )?,
        QueryExpression::DistanceTo {
            pages,
            selector,
            reference_id,
            comparison,
            distance_pt,
        } => execute_distance_to(
            &candidates,
            pages,
            &selector,
            &reference_id,
            comparison,
            distance_pt,
            &mut unavailable,
        )?,
    };
    matches.sort_by(query_match_cmp);
    let total_matches = matches.len();
    let geometry_unavailable_objects = unavailable.len();
    let warnings = if geometry_unavailable_objects == 0 {
        Vec::new()
    } else {
        vec![Diagnostic::warning(
            "SPATIAL_GEOMETRY_UNAVAILABLE",
            format!(
                "{geometry_unavailable_objects} selected object(s) had no canonical page geometry"
            ),
            "objects without page-point geometry were excluded from spatial relation evaluation",
        )]
    };
    Ok(SpatialQueryExecution {
        result: SpatialQueryResult {
            query: query.to_owned(),
            total_matches,
            geometry_unavailable_objects,
            matches,
        },
        warnings,
    })
}

fn expression_uses_caption(expression: &QueryExpression) -> bool {
    match expression {
        QueryExpression::Select { selector, .. } => selector.kind == SelectorKind::Caption,
        QueryExpression::Spatial {
            selector,
            reference,
            ..
        } => selector.kind == SelectorKind::Caption || reference.kind == SelectorKind::Caption,
        QueryExpression::DistanceTo { selector, .. } => selector.kind == SelectorKind::Caption,
    }
}

fn execute_relation(
    candidates: &[Candidate],
    pages: Option<PageRange>,
    selector: &Selector,
    operator: SpatialOperator,
    reference: &Selector,
    unavailable: &mut BTreeSet<String>,
) -> Result<Vec<QueryMatch>, DocsightError> {
    let selected = select_candidates(candidates, selector, pages);
    let references = select_candidates(candidates, reference, None);
    let comparisons = selected
        .len()
        .checked_mul(references.len())
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "DQL spatial comparisons".to_owned(),
            limit: MAX_SPATIAL_COMPARISONS as u64,
        })?;
    if comparisons > MAX_SPATIAL_COMPARISONS {
        return Err(DocsightError::ResourceLimit {
            resource: "DQL spatial comparisons".to_owned(),
            limit: MAX_SPATIAL_COMPARISONS as u64,
        });
    }
    let geometric_references = references
        .into_iter()
        .filter_map(|candidate| match candidate.geometry() {
            Some(geometry) => Some((candidate, geometry)),
            None => {
                unavailable.insert(candidate.object.id.to_string());
                None
            }
        })
        .collect::<Vec<_>>();

    Ok(selected
        .into_iter()
        .filter_map(|candidate| {
            let (page, bbox) = match candidate.geometry() {
                Some(geometry) => geometry,
                None => {
                    unavailable.insert(candidate.object.id.to_string());
                    return None;
                }
            };
            let mut applicable = geometric_references
                .iter()
                .filter(|(reference, (reference_page, reference_bbox))| {
                    candidate.object.id != reference.object.id
                        && page == *reference_page
                        && relation_matches(operator, bbox, *reference_bbox)
                })
                .map(|(reference, (_, reference_bbox))| {
                    (reference, rect_distance(bbox, *reference_bbox))
                })
                .collect::<Vec<_>>();
            applicable.sort_by(|(left, left_distance), (right, right_distance)| {
                left_distance
                    .total_cmp(right_distance)
                    .then_with(|| canonical_candidate_cmp(left, right))
            });
            let matching_anchor_count = applicable.len();
            let (reference, distance_pt) = applicable.into_iter().next()?;
            Some(QueryMatch {
                object: candidate.object.clone(),
                relation: Some(SpatialRelation {
                    kind: relation_kind(operator),
                    anchor: reference.object.id.clone(),
                    distance_pt: round_points(distance_pt),
                    matching_anchor_count,
                }),
            })
        })
        .collect())
}

fn execute_distance_to(
    candidates: &[Candidate],
    pages: Option<PageRange>,
    selector: &Selector,
    reference_id: &str,
    comparison: DistanceComparison,
    limit_pt: f32,
    unavailable: &mut BTreeSet<String>,
) -> Result<Vec<QueryMatch>, DocsightError> {
    let reference = candidates
        .iter()
        .find(|candidate| candidate.object.id.as_str() == reference_id)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: reference_id.to_owned(),
        })?;
    let (reference_page, reference_bbox) =
        reference
            .geometry()
            .ok_or_else(|| DocsightError::UnsupportedFeature {
                feature: format!(
                    "spatial query target {reference_id} has no canonical page-point geometry"
                ),
            })?;

    Ok(select_candidates(candidates, selector, pages)
        .into_iter()
        .filter_map(|candidate| {
            if candidate.object.id == reference.object.id {
                return None;
            }
            let (page, bbox) = match candidate.geometry() {
                Some(geometry) => geometry,
                None => {
                    unavailable.insert(candidate.object.id.to_string());
                    return None;
                }
            };
            if page != reference_page {
                return None;
            }
            let distance_pt = rect_distance(bbox, reference_bbox);
            let matches = match comparison {
                DistanceComparison::LessThan => distance_pt < f64::from(limit_pt),
                DistanceComparison::LessThanOrEqual => distance_pt <= f64::from(limit_pt),
            };
            matches.then(|| QueryMatch {
                object: candidate.object.clone(),
                relation: Some(SpatialRelation {
                    kind: QueryRelation::DistanceTo,
                    anchor: reference.object.id.clone(),
                    distance_pt: round_points(distance_pt),
                    matching_anchor_count: 1,
                }),
            })
        })
        .collect())
}

fn relation_matches(operator: SpatialOperator, object: Rect, anchor: Rect) -> bool {
    match operator {
        SpatialOperator::Above => {
            object.y1 <= anchor.y0 && intervals_overlap(object.x0, object.x1, anchor.x0, anchor.x1)
        }
        SpatialOperator::Below => {
            object.y0 >= anchor.y1 && intervals_overlap(object.x0, object.x1, anchor.x0, anchor.x1)
        }
        SpatialOperator::Inside => {
            object.x0 >= anchor.x0
                && object.y0 >= anchor.y0
                && object.x1 <= anchor.x1
                && object.y1 <= anchor.y1
        }
        SpatialOperator::Overlaps => object.intersects(anchor),
        SpatialOperator::Nearest => true,
    }
}

fn relation_kind(operator: SpatialOperator) -> QueryRelation {
    match operator {
        SpatialOperator::Above => QueryRelation::Above,
        SpatialOperator::Below => QueryRelation::Below,
        SpatialOperator::Inside => QueryRelation::Inside,
        SpatialOperator::Overlaps => QueryRelation::Overlaps,
        SpatialOperator::Nearest => QueryRelation::Nearest,
    }
}

fn intervals_overlap(left_start: f32, left_end: f32, right_start: f32, right_end: f32) -> bool {
    left_start < right_end && right_start < left_end
}

fn rect_distance(left: Rect, right: Rect) -> f64 {
    let dx = if left.x1 < right.x0 {
        f64::from(right.x0 - left.x1)
    } else if right.x1 < left.x0 {
        f64::from(left.x0 - right.x1)
    } else {
        0.0
    };
    let dy = if left.y1 < right.y0 {
        f64::from(right.y0 - left.y1)
    } else if right.y1 < left.y0 {
        f64::from(left.y0 - right.y1)
    } else {
        0.0
    };
    dx.hypot(dy)
}

fn round_points(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn select_candidates<'a>(
    candidates: &'a [Candidate],
    selector: &Selector,
    pages: Option<PageRange>,
) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|candidate| {
            pages.is_none_or(|range| {
                candidate
                    .object
                    .page
                    .is_some_and(|page| range.contains(page))
            }) && selector_matches(candidate, selector)
        })
        .collect()
}

fn selector_matches(candidate: &Candidate, selector: &Selector) -> bool {
    if !selector_kind_matches(candidate, selector.kind) {
        return false;
    }
    if selector
        .filters
        .iter()
        .any(|filter| !filter_matches(candidate, filter))
    {
        return false;
    }
    selector
        .contains
        .as_ref()
        .is_none_or(|needle| candidate.search_text.contains(needle))
}

fn selector_kind_matches(candidate: &Candidate, kind: SelectorKind) -> bool {
    match kind {
        SelectorKind::Object => true,
        SelectorKind::Paragraph => candidate.object.kind == SemanticKind::Paragraph,
        SelectorKind::Heading => candidate.object.kind == SemanticKind::Heading,
        SelectorKind::ListItem => candidate.object.kind == SemanticKind::ListItem,
        SelectorKind::Table => candidate.object.kind == SemanticKind::Table,
        SelectorKind::Figure => candidate.object.kind == SemanticKind::Figure,
        SelectorKind::Shape => candidate.object.kind == SemanticKind::Shape,
        SelectorKind::Note => candidate.object.kind == SemanticKind::Note,
        SelectorKind::Unknown => candidate.object.kind == SemanticKind::Unknown,
        SelectorKind::Header => candidate.object.kind == SemanticKind::Header,
        SelectorKind::Footer => candidate.object.kind == SemanticKind::Footer,
        SelectorKind::Watermark => candidate.object.kind == SemanticKind::Watermark,
        SelectorKind::CommentMarker => candidate.object.kind == SemanticKind::CommentMarker,
        SelectorKind::Annotation => candidate.object.kind == SemanticKind::Annotation,
        SelectorKind::Caption => {
            candidate.object.kind == SemanticKind::Paragraph
                && candidate
                    .style_id
                    .as_deref()
                    .is_some_and(|style| style.eq_ignore_ascii_case("caption"))
        }
    }
}

fn filter_matches(candidate: &Candidate, filter: &SelectorFilter) -> bool {
    match filter {
        SelectorFilter::HeadingLevelAtMost(level) => {
            candidate.heading_level.is_some_and(|value| value <= *level)
        }
        SelectorFilter::ConfidenceGreaterThan(value) => candidate.object.confidence > *value,
        SelectorFilter::AltEquals(value) => candidate.alt_text.as_deref() == Some(value.as_str()),
        SelectorFilter::StyleEquals(value) => candidate.style_id.as_deref() == Some(value.as_str()),
    }
}

fn candidates(document: &Document) -> Result<Vec<Candidate>, DocsightError> {
    let overlays = document.pages.iter().try_fold(0_usize, |total, page| {
        total
            .checked_add(page.overlays.len())
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "semantic viewport objects".to_owned(),
                limit: MAX_SEMANTIC_OBJECTS as u64,
            })
    })?;
    let object_count = document.blocks.len().checked_add(overlays).ok_or_else(|| {
        DocsightError::ResourceLimit {
            resource: "semantic viewport objects".to_owned(),
            limit: MAX_SEMANTIC_OBJECTS as u64,
        }
    })?;
    if object_count > MAX_SEMANTIC_OBJECTS {
        return Err(DocsightError::ResourceLimit {
            resource: "semantic viewport objects".to_owned(),
            limit: MAX_SEMANTIC_OBJECTS as u64,
        });
    }
    let mut entries = Vec::with_capacity(object_count);
    for block in &document.blocks {
        let (heading_level, style_id, alt_text, note_anchor_path) = match &block.content {
            BlockContent::Paragraph(value) => (None, value.style_id.clone(), None, None),
            BlockContent::Heading(value) => (Some(value.level), value.style_id.clone(), None, None),
            BlockContent::ListItem(value) => (None, value.style_id.clone(), None, None),
            BlockContent::Table(_) => (None, None, None, None),
            BlockContent::Figure(value) => (None, None, value.alt_text.clone(), None),
            BlockContent::Shape(_) | BlockContent::Unknown(_) => (None, None, None, None),
            BlockContent::Note(value) => (None, None, None, value.anchor_path.clone()),
        };
        let search_text = block.text();
        let (text_snippet, text_truncated) = semantic_snippet(&search_text);
        entries.push(Candidate {
            object: SemanticObject {
                id: block.id.clone(),
                kind: block.kind.into(),
                page: block.page,
                bbox: block.bbox,
                z_index: block.z_index,
                reading_order: block.reading_order,
                source: block.source.path.clone(),
                confidence: block.confidence,
                text_snippet,
                text_truncated,
            },
            search_text,
            heading_level,
            style_id,
            alt_text,
            note_anchor_path,
        });
    }
    for page in &document.pages {
        let page_max_order = document
            .page_blocks(page.number)
            .map(|block| block.reading_order)
            .max()
            .unwrap_or(0);
        for (index, overlay) in page.overlays.iter().enumerate() {
            let index = u32::try_from(index + 1).map_err(|_| DocsightError::ResourceLimit {
                resource: "page overlays".to_owned(),
                limit: u64::from(u32::MAX),
            })?;
            let reading_order =
                page_max_order
                    .checked_add(index)
                    .ok_or_else(|| DocsightError::ResourceLimit {
                        resource: "page overlay reading order".to_owned(),
                        limit: u64::from(u32::MAX),
                    })?;
            let (text_snippet, text_truncated) = semantic_snippet(&overlay.text);
            entries.push(Candidate {
                object: SemanticObject {
                    id: overlay.id.clone(),
                    kind: overlay.kind.into(),
                    page: Some(overlay.page),
                    bbox: overlay.bbox,
                    z_index: 1,
                    reading_order,
                    source: overlay.source.path.clone(),
                    confidence: 1.0,
                    text_snippet,
                    text_truncated,
                },
                search_text: overlay.text.clone(),
                heading_level: None,
                style_id: None,
                alt_text: None,
                note_anchor_path: None,
            });
        }
    }
    entries.sort_by(canonical_candidate_cmp);
    Ok(entries)
}

fn semantic_snippet(text: &str) -> (String, bool) {
    const MAX_CHARS: usize = 240;
    if text.chars().count() <= MAX_CHARS {
        return (text.to_owned(), false);
    }
    (text.chars().take(MAX_CHARS).collect(), true)
}

fn canonical_candidate_cmp(left: &Candidate, right: &Candidate) -> Ordering {
    left.object
        .page
        .unwrap_or(u32::MAX)
        .cmp(&right.object.page.unwrap_or(u32::MAX))
        .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
        .then_with(|| left.object.id.cmp(&right.object.id))
}

fn query_match_cmp(left: &QueryMatch, right: &QueryMatch) -> Ordering {
    left.object
        .page
        .unwrap_or(u32::MAX)
        .cmp(&right.object.page.unwrap_or(u32::MAX))
        .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
        .then_with(|| left.object.id.cmp(&right.object.id))
}

fn parse_query(input: &str) -> Result<QueryExpression, DocsightError> {
    let input = input.trim();
    if input.is_empty() {
        return invalid_query("query must not be empty");
    }
    if input.len() > MAX_DQL_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "DQL query bytes".to_owned(),
            limit: MAX_DQL_BYTES as u64,
        });
    }
    let (pages, input) = parse_page_scope(input)?;
    let (selector, rest) = parse_selector(input)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Ok(QueryExpression::Select { pages, selector });
    }
    if let Some(rest) = rest.strip_prefix("distance-to(") {
        let (reference_id, rest) = parse_until_close_paren(rest)?;
        validate_object_id(reference_id)?;
        let rest = rest.trim_start();
        let (comparison, rest) = if let Some(rest) = rest.strip_prefix("<=") {
            (DistanceComparison::LessThanOrEqual, rest)
        } else if let Some(rest) = rest.strip_prefix('<') {
            (DistanceComparison::LessThan, rest)
        } else {
            return invalid_query("distance-to requires '<' or '<=' followed by a point distance");
        };
        let distance_pt = parse_points(rest.trim_start())?;
        return Ok(QueryExpression::DistanceTo {
            pages,
            selector,
            reference_id: reference_id.to_owned(),
            comparison,
            distance_pt,
        });
    }
    let (operator, rest) = parse_operator(rest)?;
    let (reference_input, rest) = parse_until_close_paren(rest)?;
    if !rest.trim().is_empty() {
        return invalid_query("unexpected input after spatial relation");
    }
    let (reference, remaining) = parse_selector(reference_input)?;
    if !remaining.trim().is_empty() {
        return invalid_query("spatial relation reference must contain one selector");
    }
    Ok(QueryExpression::Spatial {
        pages,
        selector,
        operator,
        reference,
    })
}

fn parse_page_scope(input: &str) -> Result<(Option<PageRange>, &str), DocsightError> {
    let Some(rest) = input.strip_prefix("page[") else {
        return Ok((None, input));
    };
    let (value, rest) = parse_until_close_bracket(rest)?;
    if !rest.starts_with(char::is_whitespace) {
        return invalid_query("page scope must be followed by a selector");
    }
    let (start, end) = match value.split_once("..") {
        Some((start, end)) => (start, end),
        None => (value, value),
    };
    let start = start
        .parse::<u32>()
        .map_err(|_| DocsightError::InvalidArgument {
            message: "DQL page scope has an invalid start page".to_owned(),
        })?;
    let end = end
        .parse::<u32>()
        .map_err(|_| DocsightError::InvalidArgument {
            message: "DQL page scope has an invalid end page".to_owned(),
        })?;
    Ok((Some(PageRange::new(start, end)?), rest.trim_start()))
}

fn parse_selector(input: &str) -> Result<(Selector, &str), DocsightError> {
    let input = input.trim_start();
    let identifier_len = input
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_lowercase() || *character == '_')
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    if identifier_len == 0 {
        return invalid_query("selector must start with a lowercase object kind");
    }
    let kind = parse_selector_kind(&input[..identifier_len])?;
    let mut rest = &input[identifier_len..];
    let mut filters = Vec::new();
    while let Some(bracketed) = rest.strip_prefix('[') {
        let (filter, next) = parse_until_close_bracket(bracketed)?;
        filters.push(parse_filter(kind, filter)?);
        rest = next;
    }
    let rest_after_space = rest.trim_start();
    let contains = if let Some(contains_input) = rest_after_space.strip_prefix("contains(") {
        let (argument, next) = parse_until_close_paren(contains_input)?;
        let text = serde_json::from_str::<String>(argument).map_err(|_| {
            DocsightError::InvalidArgument {
                message: "contains requires a JSON string literal".to_owned(),
            }
        })?;
        rest = next;
        Some(text)
    } else {
        None
    };
    Ok((
        Selector {
            kind,
            filters,
            contains,
        },
        rest,
    ))
}

fn parse_selector_kind(input: &str) -> Result<SelectorKind, DocsightError> {
    match input {
        "object" => Ok(SelectorKind::Object),
        "paragraph" => Ok(SelectorKind::Paragraph),
        "heading" => Ok(SelectorKind::Heading),
        "list_item" => Ok(SelectorKind::ListItem),
        "table" => Ok(SelectorKind::Table),
        "figure" => Ok(SelectorKind::Figure),
        "shape" => Ok(SelectorKind::Shape),
        "note" => Ok(SelectorKind::Note),
        "unknown" => Ok(SelectorKind::Unknown),
        "header" => Ok(SelectorKind::Header),
        "footer" => Ok(SelectorKind::Footer),
        "watermark" => Ok(SelectorKind::Watermark),
        "comment_marker" => Ok(SelectorKind::CommentMarker),
        "annotation" => Ok(SelectorKind::Annotation),
        "caption" => Ok(SelectorKind::Caption),
        _ => Err(DocsightError::UnsupportedFeature {
            feature: format!("DQL selector '{input}'"),
        }),
    }
}

fn parse_filter(kind: SelectorKind, input: &str) -> Result<SelectorFilter, DocsightError> {
    if let Some(value) = input.strip_prefix("level<=") {
        if kind != SelectorKind::Heading {
            return invalid_query("level filter is valid only for heading selectors");
        }
        let value = value
            .parse::<u8>()
            .map_err(|_| DocsightError::InvalidArgument {
                message: "heading level filter must contain an integer".to_owned(),
            })?;
        if value == 0 {
            return invalid_query("heading level filter must be at least 1");
        }
        return Ok(SelectorFilter::HeadingLevelAtMost(value));
    }
    if let Some(value) = input.strip_prefix("confidence>") {
        let value = value
            .parse::<f32>()
            .map_err(|_| DocsightError::InvalidArgument {
                message: "confidence filter must contain a finite number".to_owned(),
            })?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return invalid_query("confidence filter must be between 0 and 1");
        }
        return Ok(SelectorFilter::ConfidenceGreaterThan(value));
    }
    if let Some(value) = input.strip_prefix("alt=") {
        if kind != SelectorKind::Figure {
            return invalid_query("alt filter is valid only for figure selectors");
        }
        let value = parse_json_string(value, "alt filter")?;
        return Ok(SelectorFilter::AltEquals(value));
    }
    if let Some(value) = input.strip_prefix("style=") {
        if !matches!(
            kind,
            SelectorKind::Paragraph | SelectorKind::Heading | SelectorKind::ListItem
        ) {
            return invalid_query("style filter is valid only for text block selectors");
        }
        let value = parse_json_string(value, "style filter")?;
        return Ok(SelectorFilter::StyleEquals(value));
    }
    invalid_query("unsupported selector filter")
}

fn parse_json_string(input: &str, label: &str) -> Result<String, DocsightError> {
    serde_json::from_str(input).map_err(|_| DocsightError::InvalidArgument {
        message: format!("{label} requires a JSON string literal"),
    })
}

fn parse_operator(input: &str) -> Result<(SpatialOperator, &str), DocsightError> {
    for (name, operator) in [
        ("above(", SpatialOperator::Above),
        ("below(", SpatialOperator::Below),
        ("inside(", SpatialOperator::Inside),
        ("overlaps(", SpatialOperator::Overlaps),
        ("nearest(", SpatialOperator::Nearest),
    ] {
        if let Some(rest) = input.strip_prefix(name) {
            return Ok((operator, rest));
        }
    }
    invalid_query("expected one of above, below, inside, overlaps, nearest, or distance-to")
}

fn parse_until_close_bracket(input: &str) -> Result<(&str, &str), DocsightError> {
    let Some(index) = input.find(']') else {
        return invalid_query("selector filter is missing a closing ']'");
    };
    Ok((&input[..index], &input[index + 1..]))
}

fn parse_until_close_paren(input: &str) -> Result<(&str, &str), DocsightError> {
    let mut quote = false;
    let mut escaped = false;
    let mut depth = 0_u32;
    for (index, character) in input.char_indices() {
        if quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quote = false;
            }
        } else if character == '"' {
            quote = true;
        } else if character == '(' {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| DocsightError::ResourceLimit {
                    resource: "DQL parenthesis nesting".to_owned(),
                    limit: 64,
                })?;
            if depth > 64 {
                return Err(DocsightError::ResourceLimit {
                    resource: "DQL parenthesis nesting".to_owned(),
                    limit: 64,
                });
            }
        } else if character == ')' {
            if depth == 0 {
                return Ok((&input[..index], &input[index + 1..]));
            }
            depth -= 1;
        }
    }
    invalid_query("spatial relation is missing a closing ')'")
}

fn parse_points(input: &str) -> Result<f32, DocsightError> {
    let Some(value) = input.strip_suffix("pt") else {
        return invalid_query("spatial distances must use the 'pt' unit");
    };
    let value = value.trim();
    let points = value
        .parse::<f32>()
        .map_err(|_| DocsightError::InvalidArgument {
            message: "spatial distance must be a finite positive number".to_owned(),
        })?;
    if !points.is_finite() || points <= 0.0 {
        return invalid_query("spatial distance must be a finite positive number");
    }
    Ok(points)
}

fn validate_object_id(value: &str) -> Result<(), DocsightError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return invalid_query("distance-to requires a canonical object ID");
    }
    Ok(())
}

fn invalid_query<T>(message: &str) -> Result<T, DocsightError> {
    Err(DocsightError::InvalidArgument {
        message: format!("invalid DQL: {message}"),
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverviewCounts {
    pub blocks: usize,
    pub headings: usize,
    pub tables: usize,
    pub figures: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverviewResult {
    pub format: DocumentFormat,
    pub page_count: usize,
    pub counts: OverviewCounts,
    pub total_landmarks: usize,
    pub landmarks: Vec<SemanticObject>,
}

pub fn overview(document: &Document) -> Result<OverviewResult, DocsightError> {
    let candidates = candidates(document)?;
    let landmarks = candidates
        .into_iter()
        .filter(|candidate| {
            matches!(
                candidate.object.kind,
                SemanticKind::Heading | SemanticKind::Table | SemanticKind::Figure
            )
        })
        .map(|candidate| candidate.object)
        .collect::<Vec<_>>();
    let total_landmarks = landmarks.len();
    Ok(OverviewResult {
        format: document.format,
        page_count: document.pages.len(),
        counts: OverviewCounts {
            blocks: document.blocks.len(),
            headings: document.headings().count(),
            tables: document.tables().count(),
            figures: document.figures().count(),
        },
        total_landmarks,
        landmarks,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ViewportTarget {
    Object { id: ObjectId },
    PageRange { start_page: u32, end_page: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewportRole {
    Target,
    PageRange,
    ParentHeading,
    Previous,
    Next,
    RelatedCaption,
    RelatedNote,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportRelationship {
    pub role: ViewportRole,
    pub confidence: f32,
    pub provenance: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportObject {
    pub object: SemanticObject,
    pub relationships: Vec<ViewportRelationship>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisualReference {
    pub page: u32,
    pub bbox: Rect,
    pub dpi: u16,
    pub command: String,
    pub artifact_available: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticViewport {
    pub target: ViewportTarget,
    pub scope_pages: Vec<u32>,
    pub total_objects: usize,
    pub objects: Vec<ViewportObject>,
    pub visual_references: Vec<VisualReference>,
}

pub fn focus_object(
    document: &Document,
    object_id: &str,
    include_related: bool,
) -> Result<SemanticViewport, DocsightError> {
    let candidates = candidates(document)?;
    let target_index = candidates
        .iter()
        .position(|candidate| candidate.object.id.as_str() == object_id)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: object_id.to_owned(),
        })?;
    let target = &candidates[target_index];
    let (target_page, target_bbox) =
        target
            .geometry()
            .ok_or_else(|| DocsightError::UnsupportedFeature {
                feature: format!(
                    "semantic viewport target {object_id} has no canonical page-point geometry"
                ),
            })?;
    let mut selected = BTreeMap::new();
    add_viewport_entry(
        &mut selected,
        target,
        ViewportRelationship {
            role: ViewportRole::Target,
            confidence: 1.0,
            provenance: "explicit_object_id".to_owned(),
        },
    );
    if let Some(parent) = parent_heading(&candidates, target_index, target) {
        add_viewport_entry(
            &mut selected,
            parent,
            ViewportRelationship {
                role: ViewportRole::ParentHeading,
                confidence: 0.75,
                provenance: "heading_level_and_reading_order".to_owned(),
            },
        );
    }
    let page_candidates = candidates
        .iter()
        .filter(|candidate| candidate.object.page == Some(target_page))
        .collect::<Vec<_>>();
    if let Some(position) = page_candidates
        .iter()
        .position(|candidate| candidate.object.id == target.object.id)
    {
        if let Some(previous) = position
            .checked_sub(1)
            .and_then(|index| page_candidates.get(index))
        {
            add_viewport_entry(
                &mut selected,
                previous,
                ViewportRelationship {
                    role: ViewportRole::Previous,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
        if let Some(next) = page_candidates.get(position + 1) {
            add_viewport_entry(
                &mut selected,
                next,
                ViewportRelationship {
                    role: ViewportRole::Next,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
    }
    if include_related {
        if let Some(caption) = page_candidates
            .iter()
            .filter(|candidate| {
                candidate.object.id != target.object.id
                    && selector_kind_matches(candidate, SelectorKind::Caption)
                    && candidate.geometry().is_some()
            })
            .min_by(|left, right| {
                let left_distance = left
                    .geometry()
                    .map(|(_, bbox)| rect_distance(target_bbox, bbox))
                    .unwrap_or(f64::INFINITY);
                let right_distance = right
                    .geometry()
                    .map(|(_, bbox)| rect_distance(target_bbox, bbox))
                    .unwrap_or(f64::INFINITY);
                left_distance
                    .total_cmp(&right_distance)
                    .then_with(|| canonical_candidate_cmp(left, right))
            })
        {
            add_viewport_entry(
                &mut selected,
                caption,
                ViewportRelationship {
                    role: ViewportRole::RelatedCaption,
                    confidence: 0.5,
                    provenance: "same_page_nearest_caption_geometry".to_owned(),
                },
            );
        }
        for note in page_candidates.iter().filter(|candidate| {
            candidate.object.kind == SemanticKind::Note
                && candidate.note_anchor_path.as_deref() == Some(target.object.source.as_str())
        }) {
            add_viewport_entry(
                &mut selected,
                note,
                ViewportRelationship {
                    role: ViewportRole::RelatedNote,
                    confidence: 1.0,
                    provenance: "source_anchor_path".to_owned(),
                },
            );
        }
    }
    let mut objects = selected.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| {
        left.object
            .page
            .unwrap_or(u32::MAX)
            .cmp(&right.object.page.unwrap_or(u32::MAX))
            .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
            .then_with(|| left.object.id.cmp(&right.object.id))
    });
    let total_objects = objects.len();
    Ok(SemanticViewport {
        target: ViewportTarget::Object {
            id: target.object.id.clone(),
        },
        scope_pages: vec![target_page],
        total_objects,
        objects,
        visual_references: vec![visual_reference(target_page, target_bbox)],
    })
}

pub fn focus_pages(
    document: &Document,
    pages: PageRange,
) -> Result<SemanticViewport, DocsightError> {
    let candidates = candidates(document)?;
    let scope_pages = document
        .pages
        .iter()
        .filter(|page| pages.contains(page.number))
        .map(|page| page.number)
        .collect::<Vec<_>>();
    if scope_pages.is_empty() {
        return Err(DocsightError::ObjectNotFound {
            object: format!("page range {}..{}", pages.start, pages.end),
        });
    }
    if scope_pages.len() > MAX_VIEWPORT_PAGES {
        return Err(DocsightError::ResourceLimit {
            resource: "semantic viewport pages".to_owned(),
            limit: MAX_VIEWPORT_PAGES as u64,
        });
    }
    let mut selected = BTreeMap::new();
    for candidate in candidates.iter().filter(|candidate| {
        candidate
            .object
            .page
            .is_some_and(|page| pages.contains(page))
    }) {
        add_viewport_entry(
            &mut selected,
            candidate,
            ViewportRelationship {
                role: ViewportRole::PageRange,
                confidence: 1.0,
                provenance: "explicit_page_range".to_owned(),
            },
        );
    }
    let first_index = candidates.iter().position(|candidate| {
        candidate
            .object
            .page
            .is_some_and(|page| pages.contains(page))
    });
    let last_index = candidates.iter().rposition(|candidate| {
        candidate
            .object
            .page
            .is_some_and(|page| pages.contains(page))
    });
    if let (Some(first_index), Some(last_index)) = (first_index, last_index) {
        if let Some(parent) = candidates[..first_index]
            .iter()
            .rev()
            .find(|candidate| candidate.object.kind == SemanticKind::Heading)
        {
            add_viewport_entry(
                &mut selected,
                parent,
                ViewportRelationship {
                    role: ViewportRole::ParentHeading,
                    confidence: 0.75,
                    provenance: "heading_reading_order_before_page_range".to_owned(),
                },
            );
        }
        if let Some(previous) = first_index
            .checked_sub(1)
            .and_then(|index| candidates.get(index))
        {
            add_viewport_entry(
                &mut selected,
                previous,
                ViewportRelationship {
                    role: ViewportRole::Previous,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
        if let Some(next) = last_index
            .checked_add(1)
            .and_then(|index| candidates.get(index))
        {
            add_viewport_entry(
                &mut selected,
                next,
                ViewportRelationship {
                    role: ViewportRole::Next,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
    }
    let mut objects = selected.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| {
        left.object
            .page
            .unwrap_or(u32::MAX)
            .cmp(&right.object.page.unwrap_or(u32::MAX))
            .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
            .then_with(|| left.object.id.cmp(&right.object.id))
    });
    let total_objects = objects.len();
    let visual_references = document
        .pages
        .iter()
        .filter(|page| pages.contains(page.number))
        .map(|page| {
            Rect::new(0.0, 0.0, page.width_pt, page.height_pt)
                .map(|bbox| visual_reference(page.number, bbox))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SemanticViewport {
        target: ViewportTarget::PageRange {
            start_page: pages.start,
            end_page: pages.end,
        },
        scope_pages,
        total_objects,
        objects,
        visual_references,
    })
}

fn add_viewport_entry(
    entries: &mut BTreeMap<String, ViewportObject>,
    candidate: &Candidate,
    relationship: ViewportRelationship,
) {
    entries
        .entry(candidate.object.id.to_string())
        .and_modify(|entry| entry.relationships.push(relationship.clone()))
        .or_insert_with(|| ViewportObject {
            object: candidate.object.clone(),
            relationships: vec![relationship],
        });
}

fn parent_heading<'a>(
    candidates: &'a [Candidate],
    target_index: usize,
    target: &Candidate,
) -> Option<&'a Candidate> {
    let target_level = target.heading_level;
    candidates[..target_index].iter().rev().find(|candidate| {
        candidate.object.kind == SemanticKind::Heading
            && target_level.is_none_or(|target_level| {
                candidate
                    .heading_level
                    .is_some_and(|level| level < target_level)
            })
    })
}

fn visual_reference(page: u32, bbox: Rect) -> VisualReference {
    VisualReference {
        page,
        bbox,
        dpi: 36,
        command: "crop".to_owned(),
        artifact_available: false,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PeekTarget {
    Page { page: u32 },
    PageRange { start_page: u32, end_page: u32 },
    Object { id: ObjectId },
    Section { index: u32, id: ObjectId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeekResult {
    pub target: PeekTarget,
    pub scope_pages: Vec<u32>,
    pub total_objects: usize,
    pub objects: Vec<ViewportObject>,
}

pub fn peek_object(
    document: &Document,
    object_id: &str,
    include_related: bool,
) -> Result<PeekResult, DocsightError> {
    let viewport = context_neighborhood(document, object_id, include_related)?;
    Ok(PeekResult {
        target: PeekTarget::Object {
            id: ObjectId::from_raw(object_id),
        },
        scope_pages: viewport.scope_pages,
        total_objects: viewport.total_objects,
        objects: viewport.objects,
    })
}

pub fn peek_pages(document: &Document, pages: PageRange) -> Result<PeekResult, DocsightError> {
    let scope_count = document
        .pages
        .iter()
        .filter(|page| pages.contains(page.number))
        .count();
    if scope_count > MAX_PEEK_PAGES {
        return Err(DocsightError::ResourceLimit {
            resource: "peek pages".to_owned(),
            limit: MAX_PEEK_PAGES as u64,
        });
    }
    let viewport = focus_pages(document, pages)?;
    let target = if pages.start == pages.end {
        PeekTarget::Page { page: pages.start }
    } else {
        PeekTarget::PageRange {
            start_page: pages.start,
            end_page: pages.end,
        }
    };
    Ok(PeekResult {
        target,
        scope_pages: viewport.scope_pages,
        total_objects: viewport.total_objects,
        objects: viewport.objects,
    })
}

pub fn peek_section(document: &Document, index: u32) -> Result<PeekResult, DocsightError> {
    if index == 0 {
        return Err(DocsightError::InvalidArgument {
            message: "peek --section uses one-based section indexes".to_owned(),
        });
    }
    if document.format == DocumentFormat::Pdf {
        return Err(DocsightError::UnsupportedFeature {
            feature: "peek section targeting for PDF documents".to_owned(),
        });
    }
    let section = document
        .sections
        .iter()
        .find(|section| section.section_index == index)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("section {index}"),
        })?;
    if document.sections.len() != 1
        || document
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_SECTIONS_COLLAPSED")
    {
        return Err(DocsightError::UnsupportedFeature {
            feature: "peek section page mapping for multi-section documents".to_owned(),
        });
    }
    let first_page = document
        .pages
        .first()
        .map(|page| page.number)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("section {index} pages"),
        })?;
    let last_page = document
        .pages
        .last()
        .map(|page| page.number)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: format!("section {index} pages"),
        })?;
    let mut result = peek_pages(document, PageRange::new(first_page, last_page)?)?;
    result.target = PeekTarget::Section {
        index,
        id: section.id.clone(),
    };
    Ok(result)
}

pub fn context_neighborhood(
    document: &Document,
    object_id: &str,
    include_related: bool,
) -> Result<SemanticViewport, DocsightError> {
    let candidates = candidates(document)?;
    let target_index = candidates
        .iter()
        .position(|candidate| candidate.object.id.as_str() == object_id)
        .ok_or_else(|| DocsightError::ObjectNotFound {
            object: object_id.to_owned(),
        })?;
    let target = &candidates[target_index];
    let mut selected = BTreeMap::new();
    add_viewport_entry(
        &mut selected,
        target,
        ViewportRelationship {
            role: ViewportRole::Target,
            confidence: 1.0,
            provenance: "explicit_object_id".to_owned(),
        },
    );
    if let Some(parent) = parent_heading(&candidates, target_index, target) {
        add_viewport_entry(
            &mut selected,
            parent,
            ViewportRelationship {
                role: ViewportRole::ParentHeading,
                confidence: 0.75,
                provenance: "heading_level_and_reading_order".to_owned(),
            },
        );
    }
    let page_candidates = candidates
        .iter()
        .filter(|candidate| candidate.object.page == target.object.page)
        .collect::<Vec<_>>();
    if let Some(position) = page_candidates
        .iter()
        .position(|candidate| candidate.object.id == target.object.id)
    {
        if let Some(previous) = position
            .checked_sub(1)
            .and_then(|index| page_candidates.get(index))
        {
            add_viewport_entry(
                &mut selected,
                previous,
                ViewportRelationship {
                    role: ViewportRole::Previous,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
        if let Some(next) = page_candidates.get(position + 1) {
            add_viewport_entry(
                &mut selected,
                next,
                ViewportRelationship {
                    role: ViewportRole::Next,
                    confidence: 1.0,
                    provenance: "canonical_reading_order".to_owned(),
                },
            );
        }
    }
    if include_related {
        if let Some(target_geometry) = target.geometry()
            && let Some(caption) = nearest_caption(&page_candidates, target, target_geometry.1)
        {
            add_viewport_entry(
                &mut selected,
                caption,
                ViewportRelationship {
                    role: ViewportRole::RelatedCaption,
                    confidence: 0.5,
                    provenance: "same_page_nearest_caption_geometry".to_owned(),
                },
            );
        }
        for note in page_candidates.iter().filter(|candidate| {
            candidate.object.kind == SemanticKind::Note
                && candidate.note_anchor_path.as_deref() == Some(target.object.source.as_str())
        }) {
            add_viewport_entry(
                &mut selected,
                note,
                ViewportRelationship {
                    role: ViewportRole::RelatedNote,
                    confidence: 1.0,
                    provenance: "source_anchor_path".to_owned(),
                },
            );
        }
    }
    let mut objects = selected.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| {
        left.object
            .page
            .unwrap_or(u32::MAX)
            .cmp(&right.object.page.unwrap_or(u32::MAX))
            .then_with(|| left.object.reading_order.cmp(&right.object.reading_order))
            .then_with(|| left.object.id.cmp(&right.object.id))
    });
    let scope_pages = target.object.page.into_iter().collect::<Vec<_>>();
    let visual_references = target
        .geometry()
        .map(|(page, bbox)| vec![visual_reference(page, bbox)])
        .unwrap_or_default();
    Ok(SemanticViewport {
        target: ViewportTarget::Object {
            id: target.object.id.clone(),
        },
        scope_pages,
        total_objects: objects.len(),
        objects,
        visual_references,
    })
}

fn nearest_caption<'a>(
    page_candidates: &[&'a Candidate],
    target: &Candidate,
    target_bbox: Rect,
) -> Option<&'a Candidate> {
    page_candidates
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.object.id != target.object.id
                && selector_kind_matches(candidate, SelectorKind::Caption)
                && candidate.geometry().is_some()
        })
        .min_by(|left, right| {
            let left_distance = left
                .geometry()
                .map(|(_, bbox)| rect_distance(target_bbox, bbox))
                .unwrap_or(f64::INFINITY);
            let right_distance = right
                .geometry()
                .map(|(_, bbox)| rect_distance(target_bbox, bbox))
                .unwrap_or(f64::INFINITY);
            left_distance
                .total_cmp(&right_distance)
                .then_with(|| canonical_candidate_cmp(left, right))
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveStatus {
    Resolved,
    LowConfidence,
    Ambiguous,
    NoMatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveReasonCode {
    ExplicitObjectId,
    DirectText,
    TokenOverlap,
    KindConstraint,
    CaptionText,
    HeadingText,
    PageConstraint,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolveReason {
    pub code: ResolveReasonCode,
    pub score: f64,
    pub weight: f64,
    pub contribution: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextMatch {
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolveCandidate {
    pub object: SemanticObject,
    pub score: f64,
    pub reasons: Vec<ResolveReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_range: Option<TextMatch>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolveQuery {
    pub text: String,
    pub normalized_text: String,
    pub tokens: Vec<String>,
    pub matching: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<SemanticKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<PageRange>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolveResult {
    pub query: ResolveQuery,
    pub status: ResolveStatus,
    pub total_candidates: usize,
    pub candidates: Vec<ResolveCandidate>,
}

pub fn resolve(
    document: &Document,
    text: &str,
    kind: Option<SemanticKind>,
    pages: Option<PageRange>,
) -> Result<ResolveResult, DocsightError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "resolve --text must not be empty".to_owned(),
        });
    }
    if text.len() > MAX_RESOLVE_TEXT_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "resolve text bytes".to_owned(),
            limit: MAX_RESOLVE_TEXT_BYTES as u64,
        });
    }
    let normalized_query = normalize_lexical(text);
    if normalized_query.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "resolve --text must contain letters or numbers".to_owned(),
        });
    }
    let candidates = candidates(document)?;
    let mut ranked = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            kind.is_none_or(|kind| candidate.object.kind == kind)
                && pages.is_none_or(|pages| {
                    candidate
                        .object
                        .page
                        .is_some_and(|page| pages.contains(page))
                })
        })
        .filter_map(|(index, candidate)| {
            score_candidate(
                &candidates,
                index,
                candidate,
                text,
                &normalized_query,
                kind,
                pages,
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| semantic_object_cmp(&left.object, &right.object))
    });
    let status = match ranked.as_slice() {
        [] => ResolveStatus::NoMatch,
        [first, ..] if first.score < RESOLVE_THRESHOLD => ResolveStatus::LowConfidence,
        [first, second, ..] if first.score - second.score <= RESOLVE_AMBIGUITY_MARGIN => {
            ResolveStatus::Ambiguous
        }
        _ => ResolveStatus::Resolved,
    };
    Ok(ResolveResult {
        query: ResolveQuery {
            text: text.to_owned(),
            tokens: normalized_query
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            normalized_text: normalized_query,
            matching: RESOLVE_MATCHING.to_owned(),
            kind,
            pages,
        },
        status,
        total_candidates: ranked.len(),
        candidates: ranked,
    })
}

fn score_candidate(
    candidates: &[Candidate],
    index: usize,
    candidate: &Candidate,
    query: &str,
    normalized_query: &str,
    kind: Option<SemanticKind>,
    pages: Option<PageRange>,
) -> Option<ResolveCandidate> {
    let normalized_text = normalize_lexical(&candidate.search_text);
    let matched_range = find_casefold_range(&candidate.search_text, query)
        .or_else(|| find_normalized_range(&candidate.search_text, query));
    let direct_score = if normalized_text == normalized_query {
        1.0
    } else if normalized_text.contains(normalized_query) || matched_range.is_some() {
        0.9
    } else {
        0.0
    };
    let token_score = token_overlap(normalized_query, &normalized_text);
    let (caption_score, caption_evidence) =
        related_caption_score(candidates, candidate, normalized_query);
    let (heading_score, heading_evidence) =
        related_heading_score(candidates, index, normalized_query);
    if direct_score == 0.0 && token_score == 0.0 && caption_score == 0.0 && heading_score == 0.0 {
        return None;
    }
    let mut reasons = vec![
        resolve_reason(
            ResolveReasonCode::DirectText,
            direct_score,
            0.5,
            matched_range
                .as_ref()
                .map(|_| "normalized_substring".to_owned()),
        ),
        resolve_reason(
            ResolveReasonCode::TokenOverlap,
            token_score,
            0.2,
            (token_score > 0.0).then(|| "normalized_alphanumeric_tokens".to_owned()),
        ),
        resolve_reason(
            ResolveReasonCode::CaptionText,
            caption_score,
            0.15,
            caption_evidence,
        ),
        resolve_reason(
            ResolveReasonCode::HeadingText,
            heading_score,
            0.1,
            heading_evidence,
        ),
    ];
    if kind.is_some() {
        reasons.push(resolve_reason(
            ResolveReasonCode::KindConstraint,
            1.0,
            0.03,
            Some(format!("{:?}", candidate.object.kind).to_lowercase()),
        ));
    }
    if pages.is_some() {
        reasons.push(resolve_reason(
            ResolveReasonCode::PageConstraint,
            1.0,
            0.02,
            candidate.object.page.map(|page| format!("page:{page}")),
        ));
    }
    let score = round_score(reasons.iter().map(|reason| reason.contribution).sum());
    if score == 0.0 {
        return None;
    }
    Some(ResolveCandidate {
        object: candidate.object.clone(),
        score,
        reasons,
        matched_range,
    })
}

fn resolve_reason(
    code: ResolveReasonCode,
    score: f64,
    weight: f64,
    evidence: Option<String>,
) -> ResolveReason {
    ResolveReason {
        code,
        score: round_score(score),
        weight,
        contribution: round_score(score * weight),
        evidence,
    }
}

fn related_caption_score(
    candidates: &[Candidate],
    candidate: &Candidate,
    query: &str,
) -> (f64, Option<String>) {
    let Some(page) = candidate.object.page else {
        return (0.0, None);
    };
    candidates
        .iter()
        .filter(|other| {
            other.object.page == Some(page)
                && other.object.id != candidate.object.id
                && selector_kind_matches(other, SelectorKind::Caption)
        })
        .filter_map(|caption| {
            let lexical = lexical_relation_score(query, &caption.search_text);
            if lexical == 0.0 {
                return None;
            }
            let proximity = match (candidate.geometry(), caption.geometry()) {
                (Some((_, candidate_bbox)), Some((_, caption_bbox))) => {
                    1.0 / (1.0 + rect_distance(candidate_bbox, caption_bbox) / 72.0)
                }
                _ => 0.5,
            };
            Some((lexical * proximity, caption))
        })
        .max_by(|(left_score, left), (right_score, right)| {
            left_score
                .total_cmp(right_score)
                .then_with(|| canonical_candidate_cmp(right, left))
        })
        .map(|(score, caption)| (score, Some(caption.object.id.to_string())))
        .unwrap_or((0.0, None))
}

fn related_heading_score(
    candidates: &[Candidate],
    index: usize,
    query: &str,
) -> (f64, Option<String>) {
    candidates[..index]
        .iter()
        .rev()
        .find(|candidate| candidate.object.kind == SemanticKind::Heading)
        .map(|heading| {
            let score = lexical_relation_score(query, &heading.search_text);
            (score, (score > 0.0).then(|| heading.object.id.to_string()))
        })
        .unwrap_or((0.0, None))
}

fn lexical_relation_score(query: &str, text: &str) -> f64 {
    let text = normalize_lexical(text);
    if query.is_empty() || text.is_empty() {
        return 0.0;
    }
    if text == query {
        1.0
    } else if text.contains(query) || query.contains(&text) {
        0.9
    } else {
        token_overlap(query, &text)
    }
}

fn fold_diacritic(character: char) -> Option<char> {
    if matches!(character, '\u{0300}'..='\u{036f}') {
        return None;
    }
    Some(match character {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' | 'ð' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' | 'ĸ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    })
}

fn normalize_lexical(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in value
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(fold_diacritic)
    {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

fn token_overlap(query: &str, text: &str) -> f64 {
    let query_tokens = query.split_whitespace().collect::<BTreeSet<_>>();
    if query_tokens.is_empty() {
        return 0.0;
    }
    let text_tokens = text.split_whitespace().collect::<BTreeSet<_>>();
    let matches = query_tokens.intersection(&text_tokens).count();
    matches as f64 / query_tokens.len() as f64
}

fn find_casefold_range(text: &str, query: &str) -> Option<TextMatch> {
    let (text_folded, text_map) = casefold_chars(text);
    let (query_folded, _) = casefold_chars(query);
    if query_folded.is_empty() || query_folded.len() > text_folded.len() {
        return None;
    }
    let start = text_folded
        .windows(query_folded.len())
        .position(|window| window == query_folded.as_slice())?;
    let start_char = *text_map.get(start)?;
    let end_char = text_map
        .get(start + query_folded.len() - 1)?
        .checked_add(1)?;
    let matched = text
        .chars()
        .skip(start_char)
        .take(end_char - start_char)
        .collect();
    Some(TextMatch {
        start_char,
        end_char,
        text: matched,
    })
}

fn find_normalized_range(text: &str, query: &str) -> Option<TextMatch> {
    let (text_normalized, text_map) = normalized_chars_with_map(text);
    let (query_normalized, _) = normalized_chars_with_map(query);
    if query_normalized.is_empty() || query_normalized.len() > text_normalized.len() {
        return None;
    }
    let start = text_normalized
        .windows(query_normalized.len())
        .position(|window| window == query_normalized.as_slice())?;
    let start_char = *text_map.get(start)?;
    let end_char = text_map
        .get(start + query_normalized.len() - 1)?
        .checked_add(1)?;
    let matched = text
        .chars()
        .skip(start_char)
        .take(end_char - start_char)
        .collect();
    Some(TextMatch {
        start_char,
        end_char,
        text: matched,
    })
}

fn normalized_chars_with_map(value: &str) -> (Vec<char>, Vec<usize>) {
    let mut normalized = Vec::new();
    let mut source_indices = Vec::new();
    let mut pending_space = false;
    for (index, character) in value.chars().enumerate() {
        for lowered in character.to_lowercase().filter_map(fold_diacritic) {
            if lowered.is_alphanumeric() {
                if pending_space && !normalized.is_empty() {
                    normalized.push(' ');
                    source_indices.push(index);
                }
                normalized.push(lowered);
                source_indices.push(index);
                pending_space = false;
            } else {
                pending_space = true;
            }
        }
    }
    (normalized, source_indices)
}

fn casefold_chars(value: &str) -> (Vec<char>, Vec<usize>) {
    let mut folded = Vec::new();
    let mut source_indices = Vec::new();
    for (index, character) in value.chars().enumerate() {
        for lowered in character.to_lowercase() {
            folded.push(lowered);
            source_indices.push(index);
        }
    }
    (folded, source_indices)
}

fn round_score(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn semantic_object_cmp(left: &SemanticObject, right: &SemanticObject) -> Ordering {
    left.page
        .unwrap_or(u32::MAX)
        .cmp(&right.page.unwrap_or(u32::MAX))
        .then_with(|| left.reading_order.cmp(&right.reading_order))
        .then_with(|| left.id.cmp(&right.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use docsight_core::{
        Block, DocumentMetadata, HeadingBlock, LayoutFlags, Overlay, Page, ParagraphBlock,
        SourceSpan, TableBlock, TableCell, TrackedChanges,
    };

    fn source(path: &str) -> SourceSpan {
        SourceSpan::new(path)
    }

    fn block(
        id: &str,
        kind: BlockKind,
        page: u32,
        bbox: Rect,
        reading_order: u32,
        content: BlockContent,
    ) -> Block {
        Block {
            id: ObjectId::from_raw(id),
            kind,
            page: Some(page),
            bbox: Some(bbox),
            z_index: 0,
            reading_order,
            source: source("word/document.xml"),
            confidence: 1.0,
            flags: LayoutFlags::default(),
            content,
        }
    }

    fn document() -> Result<Document, DocsightError> {
        let heading = block(
            "h_revenue",
            BlockKind::Heading,
            1,
            Rect::new(10.0, 10.0, 100.0, 20.0)?,
            1,
            BlockContent::Heading(HeadingBlock {
                level: 1,
                text: "Revenue".to_owned(),
                style_id: Some("Heading1".to_owned()),
            }),
        );
        let paragraph = block(
            "p_intro",
            BlockKind::Paragraph,
            1,
            Rect::new(10.0, 24.0, 100.0, 34.0)?,
            2,
            BlockContent::Paragraph(ParagraphBlock {
                text: "Quarterly results".to_owned(),
                style_id: None,
            }),
        );
        let table = block(
            "tbl_revenue",
            BlockKind::Table,
            1,
            Rect::new(10.0, 40.0, 100.0, 80.0)?,
            3,
            BlockContent::Table(TableBlock {
                rows: 1,
                columns: 1,
                header_rows: 0,
                cells: Vec::new(),
                column_widths_pt: None,
                detector: None,
            }),
        );
        let table_paragraph = block(
            "p_table_cell",
            BlockKind::Paragraph,
            1,
            Rect::new(20.0, 50.0, 60.0, 60.0)?,
            4,
            BlockContent::Paragraph(ParagraphBlock {
                text: "Cell evidence".to_owned(),
                style_id: None,
            }),
        );
        let caption = block(
            "p_caption",
            BlockKind::Paragraph,
            1,
            Rect::new(10.0, 90.0, 100.0, 100.0)?,
            5,
            BlockContent::Paragraph(ParagraphBlock {
                text: "Figure caption".to_owned(),
                style_id: Some("Caption".to_owned()),
            }),
        );
        Ok(Document {
            version: docsight_core::IrVersion::current(),
            id: "doc_test".to_owned(),
            sha256: "0".repeat(64),
            format: DocumentFormat::Docx,
            size_bytes: 1,
            metadata: DocumentMetadata::default(),
            styles: Vec::new(),
            sections: Vec::new(),
            pages: vec![Page {
                number: 1,
                width_pt: 120.0,
                height_pt: 120.0,
                block_ids: vec![
                    heading.id.clone(),
                    paragraph.id.clone(),
                    table.id.clone(),
                    table_paragraph.id.clone(),
                    caption.id.clone(),
                ],
                overlays: vec![Overlay {
                    id: ObjectId::from_raw("wm_page"),
                    kind: OverlayKind::Watermark,
                    page: 1,
                    bbox: Some(Rect::new(10.0, 35.0, 100.0, 85.0)?),
                    text: "DRAFT".to_owned(),
                    source: source("word/header1.xml"),
                }],
            }],
            blocks: vec![heading, paragraph, table, table_paragraph, caption],
            resources: Vec::new(),
            links: Vec::new(),
            comments: Vec::new(),
            tracked_changes: TrackedChanges::default(),
            warnings: Vec::new(),
        })
    }

    #[test]
    fn query_below_uses_canonical_page_geometry() -> Result<(), Box<dyn std::error::Error>> {
        let result =
            execute_spatial_query(&document()?, "table below(heading contains(\"Revenue\"))")?;
        assert_eq!(result.result.total_matches, 1);
        assert_eq!(result.result.matches[0].object.id.as_str(), "tbl_revenue");
        assert_eq!(
            result.result.matches[0]
                .relation
                .as_ref()
                .ok_or("missing relation")?
                .anchor
                .as_str(),
            "h_revenue"
        );
        Ok(())
    }

    #[test]
    fn distance_query_requires_points() -> Result<(), Box<dyn std::error::Error>> {
        let result = execute_spatial_query(&document()?, "object distance-to(tbl_revenue) < 24px");
        assert!(matches!(result, Err(DocsightError::InvalidArgument { .. })));
        Ok(())
    }

    #[test]
    fn spatial_operators_return_explicit_anchor_relations() -> Result<(), Box<dyn std::error::Error>>
    {
        let document = document()?;
        let above = execute_spatial_query(&document, "paragraph above(table)")?;
        assert!(
            above
                .result
                .matches
                .iter()
                .any(|item| item.object.id.as_str() == "p_intro")
        );
        let inside = execute_spatial_query(&document, "paragraph inside(table)")?;
        assert!(
            inside
                .result
                .matches
                .iter()
                .any(|item| item.object.id.as_str() == "p_table_cell")
        );
        let overlaps = execute_spatial_query(&document, "table overlaps(watermark)")?;
        assert_eq!(overlaps.result.matches[0].object.id.as_str(), "tbl_revenue");
        assert_eq!(
            overlaps.result.matches[0]
                .relation
                .as_ref()
                .ok_or("missing relation")?
                .kind,
            QueryRelation::Overlaps
        );
        let nearest = execute_spatial_query(&document, "table nearest(caption)")?;
        assert_eq!(
            nearest.result.matches[0]
                .relation
                .as_ref()
                .ok_or("missing relation")?
                .anchor
                .as_str(),
            "p_caption"
        );
        Ok(())
    }

    #[test]
    fn fuzz_spatial_dql_parser_never_panics() -> Result<(), Box<dyn std::error::Error>> {
        let document = document()?;
        let mut state = 0x9e37_79b9_u32;
        for length in 0..512 {
            let mut input = Vec::with_capacity(length);
            for _ in 0..length {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                input.push((state >> 24) as u8);
            }
            let input = String::from_utf8_lossy(&input);
            let _ = execute_spatial_query(&document, &input);
        }
        Ok(())
    }

    #[test]
    fn focus_includes_target_parent_and_neighbors() -> Result<(), Box<dyn std::error::Error>> {
        let result = focus_object(&document()?, "tbl_revenue", false)?;
        assert_eq!(result.scope_pages, vec![1]);
        assert!(result.objects.iter().any(|entry| {
            entry.object.id.as_str() == "tbl_revenue"
                && entry
                    .relationships
                    .iter()
                    .any(|relationship| relationship.role == ViewportRole::Target)
        }));
        assert!(result.objects.iter().any(|entry| {
            entry.object.id.as_str() == "h_revenue"
                && entry
                    .relationships
                    .iter()
                    .any(|relationship| relationship.role == ViewportRole::ParentHeading)
        }));
        assert_eq!(result.visual_references[0].dpi, 36);
        assert!(!result.visual_references[0].artifact_available);
        Ok(())
    }

    #[test]
    fn focus_page_range_keeps_empty_pages_addressable() -> Result<(), Box<dyn std::error::Error>> {
        let mut document = document()?;
        document.pages.push(Page {
            number: 2,
            width_pt: 120.0,
            height_pt: 120.0,
            block_ids: Vec::new(),
            overlays: Vec::new(),
        });
        let result = focus_pages(&document, PageRange::new(2, 2)?)?;
        assert_eq!(result.scope_pages, vec![2]);
        assert!(result.objects.is_empty());
        assert_eq!(result.visual_references[0].page, 2);
        Ok(())
    }

    #[test]
    fn focus_page_range_rejects_unbounded_existing_page_sets()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut document = document()?;
        for number in 2..=33 {
            document.pages.push(Page {
                number,
                width_pt: 120.0,
                height_pt: 120.0,
                block_ids: Vec::new(),
                overlays: Vec::new(),
            });
        }
        let result = focus_pages(&document, PageRange::new(1, 33)?);
        assert!(matches!(result, Err(DocsightError::ResourceLimit { .. })));
        Ok(())
    }

    #[test]
    fn resolve_ranks_direct_text_with_explainable_components()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = resolve(
            &document()?,
            "quarterly-results",
            Some(SemanticKind::Paragraph),
            None,
        )?;
        assert_eq!(result.status, ResolveStatus::Resolved);
        assert_eq!(result.candidates[0].object.id.as_str(), "p_intro");
        assert!(result.candidates[0].matched_range.is_some());
        assert!(result.candidates[0].reasons.iter().any(|reason| {
            reason.code == ResolveReasonCode::DirectText && reason.contribution > 0.0
        }));
        assert!(result.candidates[0].reasons.iter().any(|reason| {
            reason.code == ResolveReasonCode::KindConstraint && reason.score == 1.0
        }));
        Ok(())
    }

    #[test]
    fn resolve_exposes_ties_and_absent_lexical_evidence() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut document = document()?;
        let mut duplicate = document.find_block("p_intro").ok_or("paragraph")?.clone();
        duplicate.id = ObjectId::from_raw("p_duplicate");
        duplicate.reading_order = 6;
        document.blocks.push(duplicate);
        let ambiguous = resolve(
            &document,
            "Quarterly results",
            Some(SemanticKind::Paragraph),
            None,
        )?;
        assert_eq!(ambiguous.status, ResolveStatus::Ambiguous);
        assert_eq!(ambiguous.candidates.len(), 2);

        let absent = resolve(
            &document,
            "nonexistent descriptor",
            Some(SemanticKind::Table),
            None,
        )?;
        assert_eq!(absent.status, ResolveStatus::NoMatch);
        assert!(absent.candidates.is_empty());
        Ok(())
    }

    #[test]
    fn resolve_single_weak_candidate_is_low_confidence() -> Result<(), Box<dyn std::error::Error>> {
        let paragraph = block(
            "p_due",
            BlockKind::Paragraph,
            1,
            Rect::new(10.0, 24.0, 100.0, 34.0)?,
            2,
            BlockContent::Paragraph(ParagraphBlock {
                text: "Date of issue January 1, 2026 Date due".to_owned(),
                style_id: None,
            }),
        );
        let mut document = document()?;
        document.blocks = vec![paragraph.clone()];
        document.pages[0].block_ids = vec![paragraph.id.clone()];
        let result = resolve(&document, "Amount due", None, None)?;
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.status, ResolveStatus::LowConfidence);
        Ok(())
    }

    #[test]
    fn resolve_full_phrase_outranks_partial_word_match() -> Result<(), Box<dyn std::error::Error>> {
        let paragraph = block(
            "p_due",
            BlockKind::Paragraph,
            1,
            Rect::new(10.0, 24.0, 100.0, 34.0)?,
            2,
            BlockContent::Paragraph(ParagraphBlock {
                text: "Date of issue January 1, 2026 Date due".to_owned(),
                style_id: None,
            }),
        );
        let table = block(
            "tbl_due",
            BlockKind::Table,
            1,
            Rect::new(10.0, 40.0, 100.0, 80.0)?,
            3,
            BlockContent::Table(TableBlock {
                rows: 1,
                columns: 1,
                header_rows: 0,
                cells: vec![TableCell {
                    id: ObjectId::from_raw("cell_due"),
                    row: 0,
                    column: 0,
                    row_span: 1,
                    column_span: 1,
                    bbox: None,
                    text: "Amount due".to_owned(),
                    blocks: Vec::new(),
                    source: source("pdf::page[1]::table::cell[0,0]"),
                }],
                column_widths_pt: None,
                detector: Some("ruled".to_owned()),
            }),
        );
        let mut document = document()?;
        document.blocks = vec![paragraph, table];
        document.pages[0].block_ids =
            vec![ObjectId::from_raw("p_due"), ObjectId::from_raw("tbl_due")];
        let result = resolve(&document, "Amount due", None, None)?;
        assert_eq!(result.status, ResolveStatus::Resolved);
        assert_eq!(result.candidates[0].object.id.as_str(), "tbl_due");
        Ok(())
    }

    #[test]
    fn peek_and_context_use_canonical_relationships_without_pixels()
    -> Result<(), Box<dyn std::error::Error>> {
        let document = document()?;
        let peek = peek_object(&document, "tbl_revenue", true)?;
        assert_eq!(
            peek.target,
            PeekTarget::Object {
                id: ObjectId::from_raw("tbl_revenue")
            }
        );
        assert!(peek.objects.iter().any(|entry| {
            entry.object.id.as_str() == "h_revenue"
                && entry
                    .relationships
                    .iter()
                    .any(|relationship| relationship.role == ViewportRole::ParentHeading)
        }));
        let context = context_neighborhood(&document, "tbl_revenue", true)?;
        assert_eq!(context.visual_references.len(), 1);
        assert!(!context.visual_references[0].artifact_available);
        Ok(())
    }
}
