use crate::TextMatch;
use docsight_core::{
    Block, BlockContent, DocsightError, Document, DocumentObject, ObjectId, Rect, TableCell,
};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_FIND_PATTERN_BYTES: usize = 4_096;
pub const MAX_FIND_REGEX_BYTES: usize = 1 << 20;
pub const MAX_FIND_MATCHES: usize = 100_000;
const MAX_FIND_NESTING: usize = 64;
const MATCH_CONTEXT_CHARS: usize = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindMode {
    Literal,
    Regex,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindObjectKind {
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

#[derive(Clone, Debug)]
pub struct FindRequest {
    pub pattern: String,
    pub mode: FindMode,
    pub ignore_case: bool,
    pub kinds: BTreeSet<FindObjectKind>,
    pub pages: Option<(u32, u32)>,
    pub region: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindMatch {
    pub object_id: ObjectId,
    pub kind: FindObjectKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
    pub matched: TextMatch,
    pub context_before: String,
    pub context_after: String,
    pub source: String,
    pub confidence: f32,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindResult {
    pub pattern: String,
    pub mode: FindMode,
    pub ignore_case: bool,
    pub total_matches: usize,
    pub searched_objects: usize,
    pub geometry_unavailable_matches: usize,
    pub matches: Vec<FindMatch>,
}

pub fn find(document: &Document, request: &FindRequest) -> Result<FindResult, DocsightError> {
    if request.pattern.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "find pattern must not be empty".to_owned(),
        });
    }
    if request.pattern.len() > MAX_FIND_PATTERN_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "find pattern bytes".to_owned(),
            limit: MAX_FIND_PATTERN_BYTES as u64,
        });
    }
    if let (Some(region), None) = (request.region, request.pages) {
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "a find region {region:?} requires a single page selected with --pages"
            ),
        });
    }
    if let (Some(_), Some((start, end))) = (request.region, request.pages)
        && start != end
    {
        return Err(DocsightError::InvalidArgument {
            message: "a find region applies to exactly one page".to_owned(),
        });
    }

    let matcher = build_matcher(request)?;
    let mut objects = Vec::new();
    for block in &document.blocks {
        collect_objects(block, None, &mut objects, 0);
    }
    for page in &document.pages {
        objects.extend(page.overlays.iter().map(DocumentObject::Overlay));
    }
    objects.extend(document.links.iter().map(DocumentObject::Hyperlink));

    let mut matches = Vec::new();
    let mut searched_objects = 0usize;
    for object in &objects {
        let kind = find_kind(object);
        if !request.kinds.is_empty() && !request.kinds.contains(&kind) {
            continue;
        }
        if let Some((start, end)) = request.pages {
            match object.page() {
                Some(page) if page >= start && page <= end => {}
                _ => continue,
            }
        }
        if let Some(region) = request.region {
            match object.bbox() {
                Some(bbox) if bbox.intersects(region) => {}
                _ => continue,
            }
        }
        searched_objects += 1;
        let text = object.text();
        let diagnostics = object_diagnostics(document, object);
        for (start_byte, end_byte) in matcher.ranges(&text) {
            if matches.len() >= MAX_FIND_MATCHES {
                return Err(DocsightError::ResourceLimit {
                    resource: "find matches".to_owned(),
                    limit: MAX_FIND_MATCHES as u64,
                });
            }
            matches.push(FindMatch {
                object_id: object.id().clone(),
                kind,
                container_id: object.container().map(|container| container.id.clone()),
                page: object.page(),
                bbox: object.bbox(),
                matched: TextMatch {
                    start_char: text[..start_byte].chars().count(),
                    end_char: text[..end_byte].chars().count(),
                    text: text[start_byte..end_byte].to_owned(),
                },
                context_before: tail_chars(&text[..start_byte], MATCH_CONTEXT_CHARS),
                context_after: head_chars(&text[end_byte..], MATCH_CONTEXT_CHARS),
                source: object.source().path.clone(),
                confidence: object.confidence(),
                diagnostics: diagnostics.clone(),
            });
        }
    }

    let geometry_unavailable_matches = matches
        .iter()
        .filter(|found| found.page.is_none() || found.bbox.is_none())
        .count();
    Ok(FindResult {
        pattern: request.pattern.clone(),
        mode: request.mode,
        ignore_case: request.ignore_case,
        total_matches: matches.len(),
        searched_objects,
        geometry_unavailable_matches,
        matches,
    })
}

enum Matcher {
    Literal { needle: String, ignore_case: bool },
    Regex(Regex),
}

impl Matcher {
    fn ranges(&self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Regex(regex) => regex
                .find_iter(text)
                .filter(|found| found.start() != found.end())
                .map(|found| (found.start(), found.end()))
                .collect(),
            Self::Literal {
                needle,
                ignore_case: false,
            } => text
                .match_indices(needle.as_str())
                .map(|(start, found)| (start, start + found.len()))
                .collect(),
            Self::Literal {
                needle,
                ignore_case: true,
            } => casefold_ranges(text, needle),
        }
    }
}

fn build_matcher(request: &FindRequest) -> Result<Matcher, DocsightError> {
    match request.mode {
        FindMode::Literal => Ok(Matcher::Literal {
            needle: request.pattern.clone(),
            ignore_case: request.ignore_case,
        }),
        FindMode::Regex => RegexBuilder::new(&request.pattern)
            .case_insensitive(request.ignore_case)
            .size_limit(MAX_FIND_REGEX_BYTES)
            .dfa_size_limit(MAX_FIND_REGEX_BYTES)
            .build()
            .map(Matcher::Regex)
            .map_err(|error| DocsightError::InvalidArgument {
                message: format!("find pattern is not a valid regular expression: {error}"),
            }),
    }
}

fn casefold_ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let folded_needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if folded_needle.is_empty() {
        return Vec::new();
    }
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < characters.len() {
        let mut folded = Vec::with_capacity(folded_needle.len());
        let mut cursor = index;
        while cursor < characters.len() && folded.len() < folded_needle.len() {
            folded.extend(characters[cursor].1.to_lowercase());
            cursor += 1;
        }
        if folded == folded_needle {
            let start = characters[index].0;
            let end = characters
                .get(cursor)
                .map(|(offset, _)| *offset)
                .unwrap_or(text.len());
            ranges.push((start, end));
            index = cursor;
        } else {
            index += 1;
        }
    }
    ranges
}

fn collect_objects<'a>(
    block: &'a Block,
    anchor: Option<&'a Block>,
    objects: &mut Vec<DocumentObject<'a>>,
    depth: usize,
) {
    if depth > MAX_FIND_NESTING {
        return;
    }
    let BlockContent::Table(table) = &block.content else {
        objects.push(DocumentObject::Block {
            block,
            container: anchor,
        });
        return;
    };
    let table_anchor = anchor.unwrap_or(block);
    for cell in &table.cells {
        push_cell(table_anchor, cell, objects, depth);
    }
}

fn push_cell<'a>(
    table: &'a Block,
    cell: &'a TableCell,
    objects: &mut Vec<DocumentObject<'a>>,
    depth: usize,
) {
    if cell.blocks.is_empty() {
        objects.push(DocumentObject::TableCell { table, cell });
        return;
    }
    let has_nested_table = cell
        .blocks
        .iter()
        .any(|nested| matches!(nested.content, BlockContent::Table(_)));
    if !has_nested_table {
        objects.push(DocumentObject::TableCell { table, cell });
        return;
    }
    for nested in &cell.blocks {
        collect_objects(nested, Some(table), objects, depth + 1);
    }
}

fn find_kind(object: &DocumentObject<'_>) -> FindObjectKind {
    use docsight_core::{BlockKind, OverlayKind};
    match object {
        DocumentObject::Block { block, .. } => match block.kind {
            BlockKind::Paragraph => FindObjectKind::Paragraph,
            BlockKind::Heading => FindObjectKind::Heading,
            BlockKind::ListItem => FindObjectKind::ListItem,
            BlockKind::Table => FindObjectKind::Table,
            BlockKind::Figure => FindObjectKind::Figure,
            BlockKind::Shape => FindObjectKind::Shape,
            BlockKind::Note => FindObjectKind::Note,
            BlockKind::Unknown => FindObjectKind::Unknown,
        },
        DocumentObject::TableCell { .. } => FindObjectKind::TableCell,
        DocumentObject::Overlay(overlay) => match overlay.kind {
            OverlayKind::Header => FindObjectKind::Header,
            OverlayKind::Footer => FindObjectKind::Footer,
            OverlayKind::Watermark => FindObjectKind::Watermark,
            OverlayKind::CommentMarker => FindObjectKind::CommentMarker,
            OverlayKind::Annotation => FindObjectKind::Annotation,
        },
        DocumentObject::Hyperlink(_) => FindObjectKind::Hyperlink,
    }
}

fn object_diagnostics(document: &Document, object: &DocumentObject<'_>) -> Vec<String> {
    let object_id = object.id();
    let container_id = object.container().map(|container| &container.id);
    let page = object.page();
    let codes: BTreeSet<&str> = document
        .warnings
        .iter()
        .filter(|warning| match &warning.object {
            Some(target) => target == object_id || Some(target) == container_id,
            None => warning.page.is_none() || warning.page == page,
        })
        .map(|warning| warning.code.as_str())
        .collect();
    codes.into_iter().map(str::to_owned).collect()
}

fn tail_chars(text: &str, count: usize) -> String {
    let total = text.chars().count();
    text.chars().skip(total.saturating_sub(count)).collect()
}

fn head_chars(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}
