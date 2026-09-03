use crate::package::read_parts;
use docsight_core::{
    Block, BlockContent, BlockKind, Comment, Diagnostic, DiagnosticSeverity, DocsightError,
    Document, DocumentFormat, DocumentMetadata, DocumentSource, FigureBlock, HeadingBlock,
    Hyperlink, LayoutFlags, ListItemBlock, NoteBlock, NoteKind, ObjectId, ParagraphBlock, Resource,
    ResourceKind, Section, SourceSpan, Style, TableBlock, TableCell, TrackedChanges, UnknownBlock,
};
use roxmltree::{Document as XmlDocument, Node};
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_STYLE_DEPTH: usize = 64;
const MAX_TABLE_DEPTH: usize = 32;

#[derive(Clone, Debug, Default)]
struct StyleDefinition {
    name: Option<String>,
    based_on: Option<String>,
    outline_level: Option<u8>,
    numbering: Option<NumberingProperties>,
}

#[derive(Clone, Debug, Default)]
struct NumberingProperties {
    num_id: Option<String>,
    level: Option<u8>,
}

#[derive(Clone, Debug, Default)]
struct NumberingLevel {
    format: Option<String>,
    pattern: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct NumberingDefinitions {
    numbers: BTreeMap<String, String>,
    levels: BTreeMap<(String, u8), NumberingLevel>,
}

#[derive(Clone, Debug)]
struct ListMarker {
    level: u8,
    format: Option<String>,
    pattern: Option<String>,
    ordered: Option<bool>,
}

pub fn parse_docx(source: &DocumentSource) -> Result<Document, DocsightError> {
    if source.format() != DocumentFormat::Docx {
        return Err(DocsightError::UnsupportedOperation {
            operation: "DOCX structural parsing".to_owned(),
            format: source.format(),
        });
    }
    let parts = read_parts(source.bytes())?;
    let rels = parse_relationships(parts.rels.as_deref())?;
    let header_texts = extract_part_texts(&parts.headers)?;
    let footer_texts = extract_part_texts(&parts.footers)?;
    let styles = parse_styles(parts.styles.as_deref())?;
    let numbering = parse_numbering(parts.numbering.as_deref())?;
    let xml = XmlDocument::parse(&parts.document).map_err(xml_error)?;
    let body = xml
        .descendants()
        .find(|node| node.has_tag_name((W_NS, "body")))
        .ok_or_else(|| DocsightError::MalformedDocument {
            message: "word/document.xml has no w:body".to_owned(),
        })?;
    let mut blocks = Vec::new();
    let mut sections = Vec::new();
    let mut warnings = Vec::new();
    let mut resources = Vec::new();
    let mut links = Vec::new();
    let mut paragraph_index = 0_u32;
    let mut table_index = 0_u32;
    let mut section_index = 0_u32;
    let mut figure_index = 0_u32;
    let mut unknown_index = 0_u32;
    let mut link_counter = 0_usize;
    let mut reading_order = 0_u32;
    let mut note_anchors: BTreeMap<String, String> = BTreeMap::new();
    let mut comment_anchors: BTreeMap<String, String> = BTreeMap::new();

    for child in body.children().filter(Node::is_element) {
        if child.has_tag_name((W_NS, "p")) {
            paragraph_index = paragraph_index
                .checked_add(1)
                .ok_or_else(block_count_error)?;
            let paragraph_path = format!("/word/document.xml::body/p[{paragraph_index}]");
            let child_figures = extract_figures(
                child,
                source,
                &rels,
                &mut reading_order,
                &mut figure_index,
                &mut resources,
            );
            extract_hyperlinks(
                child,
                source,
                &rels,
                &mut link_counter,
                &mut links,
                &paragraph_path,
            );
            let paragraph_blocks = parse_paragraph(
                child,
                paragraph_index,
                source,
                &styles,
                &numbering,
                &mut reading_order,
                &mut warnings,
            )?;
            blocks.extend(paragraph_blocks);
            figure_warnings(child_figures.iter(), &mut warnings);
            blocks.extend(child_figures);
            collect_anchor_ids(
                child,
                &paragraph_path,
                &mut note_anchors,
                &mut comment_anchors,
            );
        } else if child.has_tag_name((W_NS, "tbl")) {
            table_index = table_index.checked_add(1).ok_or_else(block_count_error)?;
            reading_order = reading_order.checked_add(1).ok_or_else(block_count_error)?;
            blocks.push(parse_table(child, table_index, source, reading_order)?);
        } else if child.has_tag_name((W_NS, "sectPr")) {
            section_index = section_index.checked_add(1).ok_or_else(block_count_error)?;
            sections.push(parse_section(
                child,
                section_index,
                source,
                &header_texts,
                &footer_texts,
                &rels,
            ));
        } else {
            unknown_index = unknown_index.checked_add(1).ok_or_else(block_count_error)?;
            reading_order = reading_order.checked_add(1).ok_or_else(block_count_error)?;
            blocks.push(parse_unknown_body_element(
                child,
                unknown_index,
                source,
                reading_order,
                &mut warnings,
            ));
        }
    }

    if body
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "pPr")))
        .any(|properties| child_element(properties, "sectPr").is_some())
    {
        warnings.push(Diagnostic::warning(
            "DOCX_SECTIONS_COLLAPSED",
            "document has paragraph-level section breaks that were collapsed".to_owned(),
            "only the first section geometry is applied to every page",
        ));
    }

    if sections.is_empty() {
        warnings.push(Diagnostic::warning(
            "DOCX_SECTION_DEFAULTED",
            "document.xml has no sectPr; a default section was synthesized".to_owned(),
            "page geometry falls back to Letter 612x792 pt with 72 pt margins",
        ));
        sections.push(Section {
            id: source.object_id("sect", "/word/document.xml::body/sectPr[1]"),
            section_index: 1,
            page_width_pt: Some(612.0),
            page_height_pt: Some(792.0),
            margin_top_pt: Some(72.0),
            margin_right_pt: Some(72.0),
            margin_bottom_pt: Some(72.0),
            margin_left_pt: Some(72.0),
            header_text: header_texts.first().map(|(_, t)| t.clone()),
            footer_text: footer_texts.first().map(|(_, t)| t.clone()),
        });
    }

    blocks.extend(parse_notes(
        parts.footnotes.as_deref(),
        source,
        "footnote",
        NoteKind::Footnote,
        &mut reading_order,
        &note_anchors,
    )?);
    blocks.extend(parse_notes(
        parts.endnotes.as_deref(),
        source,
        "endnote",
        NoteKind::Endnote,
        &mut reading_order,
        &note_anchors,
    )?);

    let comments = parse_comments(parts.comments.as_deref(), source, &comment_anchors)?;
    let insertions = body
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "ins")))
        .count();
    let deletions = body
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "del")))
        .count();
    let tracked_changes = TrackedChanges {
        insertions,
        deletions,
    };

    let converted_styles: Vec<Style> = styles
        .into_iter()
        .map(|(style_id, style_def)| Style {
            id: style_id,
            name: style_def.name,
            based_on: style_def.based_on,
            font_family: None,
            font_size_pt: None,
            bold: None,
            italic: None,
        })
        .collect();

    Ok(Document {
        id: source.id(),
        sha256: source.sha256().to_owned(),
        format: DocumentFormat::Docx,
        size_bytes: source.size_bytes(),
        metadata: DocumentMetadata::default(),
        styles: converted_styles,
        sections,
        pages: Vec::new(),
        blocks,
        resources,
        links,
        comments,
        tracked_changes,
        warnings,
    })
}

fn parse_section(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    header_texts: &[(String, String)],
    footer_texts: &[(String, String)],
    rels: &BTreeMap<String, (String, String)>,
) -> Section {
    let source_path = format!("/word/document.xml::body/sectPr[{index}]");
    let mut page_width_pt = None;
    let mut page_height_pt = None;
    let mut margin_top_pt = None;
    let mut margin_right_pt = None;
    let mut margin_bottom_pt = None;
    let mut margin_left_pt = None;

    if let Some(pg_sz) = child_element(node, "pgSz") {
        if let Some(w) = pg_sz
            .attribute((W_NS, "w"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            page_width_pt = Some(w / 20.0);
        }
        if let Some(h) = pg_sz
            .attribute((W_NS, "h"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            page_height_pt = Some(h / 20.0);
        }
    }
    if let Some(pg_mar) = child_element(node, "pgMar") {
        if let Some(top) = pg_mar
            .attribute((W_NS, "top"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            margin_top_pt = Some(top / 20.0);
        }
        if let Some(right) = pg_mar
            .attribute((W_NS, "right"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            margin_right_pt = Some(right / 20.0);
        }
        if let Some(bottom) = pg_mar
            .attribute((W_NS, "bottom"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            margin_bottom_pt = Some(bottom / 20.0);
        }
        if let Some(left) = pg_mar
            .attribute((W_NS, "left"))
            .and_then(|v| v.parse::<f32>().ok())
        {
            margin_left_pt = Some(left / 20.0);
        }
    }

    let mut header_text = None;
    let mut footer_text = None;

    for child in node.children().filter(Node::is_element) {
        if child.has_tag_name((W_NS, "headerReference")) {
            let r_id = child
                .attribute((R_NS, "id"))
                .or_else(|| child.attribute("r:id"));
            if let Some(r_id) = r_id {
                if let Some((_, target)) = rels.get(r_id) {
                    if let Some(normalized) = normalize_internal_target(target) {
                        if let Some((_, text)) =
                            header_texts.iter().find(|(name, _)| *name == normalized)
                        {
                            header_text = Some(text.clone());
                        }
                    }
                }
            }
        } else if child.has_tag_name((W_NS, "footerReference")) {
            let r_id = child
                .attribute((R_NS, "id"))
                .or_else(|| child.attribute("r:id"));
            if let Some(r_id) = r_id {
                if let Some((_, target)) = rels.get(r_id) {
                    if let Some(normalized) = normalize_internal_target(target) {
                        if let Some((_, text)) =
                            footer_texts.iter().find(|(name, _)| *name == normalized)
                        {
                            footer_text = Some(text.clone());
                        }
                    }
                }
            }
        }
    }

    Section {
        id: source.object_id("sect", &source_path),
        section_index: index,
        page_width_pt,
        page_height_pt,
        margin_top_pt,
        margin_right_pt,
        margin_bottom_pt,
        margin_left_pt,
        header_text,
        footer_text,
    }
}

fn normalize_internal_target(target: &str) -> Option<String> {
    if target.contains("://") || target.contains('\\') || target.contains("..") {
        return None;
    }
    let relative = target.strip_prefix('/').unwrap_or(target);
    let source_dir = "word";
    let mut segments: Vec<&str> = source_dir.split('/').collect();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments.join("/"))
}

fn parse_relationships(
    xml_opt: Option<&str>,
) -> Result<BTreeMap<String, (String, String)>, DocsightError> {
    let mut map = BTreeMap::new();
    let Some(xml) = xml_opt else {
        return Ok(map);
    };
    let doc = XmlDocument::parse(xml).map_err(xml_error)?;
    for rel in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Relationship")
    {
        if let (Some(id), Some(target)) = (rel.attribute("Id"), rel.attribute("Target")) {
            let rel_type = rel.attribute("Type").unwrap_or("").to_owned();
            map.insert(id.to_owned(), (rel_type, target.to_owned()));
        }
    }
    Ok(map)
}

fn extract_part_texts(parts: &[(String, String)]) -> Result<Vec<(String, String)>, DocsightError> {
    let mut results = Vec::new();
    for (part_name, xml_str) in parts {
        let doc = XmlDocument::parse(xml_str).map_err(xml_error)?;
        let text = doc
            .descendants()
            .filter(|n| n.has_tag_name((W_NS, "p")))
            .map(paragraph_text)
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            results.push((part_name.clone(), text));
        }
    }
    Ok(results)
}

fn extract_figures(
    node: Node<'_, '_>,
    source: &DocumentSource,
    rels: &BTreeMap<String, (String, String)>,
    reading_order: &mut u32,
    figure_index: &mut u32,
    resources: &mut Vec<Resource>,
) -> Vec<Block> {
    let mut figures = Vec::new();
    for drawing in node
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "drawing")) || n.has_tag_name((W_NS, "pict")))
    {
        *figure_index += 1;
        *reading_order += 1;
        let mut width_pt = None;
        let mut height_pt = None;
        let mut alt_text = None;
        let mut resource_id = None;

        if let Some(extent) = drawing
            .descendants()
            .find(|n| n.tag_name().name() == "extent")
        {
            if let Some(cx) = extent.attribute("cx").and_then(|v| v.parse::<f32>().ok()) {
                width_pt = Some(cx / 12700.0);
            }
            if let Some(cy) = extent.attribute("cy").and_then(|v| v.parse::<f32>().ok()) {
                height_pt = Some(cy / 12700.0);
            }
        }
        if let Some(doc_pr) = drawing
            .descendants()
            .find(|n| n.tag_name().name() == "docPr")
        {
            alt_text = doc_pr
                .attribute("descr")
                .or_else(|| doc_pr.attribute("title"))
                .or_else(|| doc_pr.attribute("name"))
                .map(str::to_owned);
        }
        if let Some(blip) = drawing
            .descendants()
            .find(|n| n.tag_name().name() == "blip")
        {
            if let Some(embed) = blip
                .attribute((R_NS, "embed"))
                .or_else(|| blip.attribute("r:embed"))
            {
                resource_id = Some(embed.to_owned());
                if let Some((_, target)) = rels.get(embed) {
                    let res_id = source.object_id("res", target);
                    if !resources.iter().any(|r| r.id == res_id) {
                        resources.push(Resource {
                            id: res_id,
                            kind: ResourceKind::Image,
                            name: target.clone(),
                            target: target.clone(),
                            mime_type: guess_mime_type(target),
                        });
                    }
                }
            }
        }

        let source_path = format!("/word/document.xml::figure[{figure_index}]");
        figures.push(Block {
            id: source.object_id("fig", &source_path),
            kind: BlockKind::Figure,
            page: None,
            bbox: None,
            z_index: 0,
            reading_order: *reading_order,
            source: SourceSpan::new(&source_path),
            confidence: 1.0,
            flags: LayoutFlags::default(),
            content: BlockContent::Figure(FigureBlock {
                alt_text,
                caption: None,
                resource_id,
                width_pt,
                height_pt,
            }),
        });
    }
    figures
}

fn figure_warnings<'a>(figures: impl Iterator<Item = &'a Block>, warnings: &mut Vec<Diagnostic>) {
    for figure in figures {
        warnings.push(Diagnostic {
            code: "DOCX_FIGURE_RASTER_PLACEHOLDER".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "embedded image for figure {} is not rasterized; a placeholder box is rendered",
                figure.id
            ),
            effect: "visual evidence for this figure does not include the original image pixels"
                .to_owned(),
            object: Some(figure.id.clone()),
            page: None,
        });
    }
}

fn collect_anchor_ids(
    paragraph: Node<'_, '_>,
    paragraph_path: &str,
    note_anchors: &mut BTreeMap<String, String>,
    comment_anchors: &mut BTreeMap<String, String>,
) {
    for descendant in paragraph.descendants().filter(Node::is_element) {
        let name = descendant.tag_name().name();
        if name != "footnoteReference" && name != "endnoteReference" && name != "commentRangeStart"
        {
            continue;
        }
        let Some(id) = descendant
            .attribute((W_NS, "id"))
            .or_else(|| descendant.attribute("w:id"))
        else {
            continue;
        };
        if id == "-1" || id == "0" {
            continue;
        }
        let target = if name == "commentRangeStart" {
            &mut *comment_anchors
        } else {
            &mut *note_anchors
        };
        target
            .entry(id.to_owned())
            .or_insert_with(|| paragraph_path.to_owned());
    }
}

fn guess_mime_type(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Some("image/png".to_owned())
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg".to_owned())
    } else if lower.ends_with(".svg") {
        Some("image/svg+xml".to_owned())
    } else if lower.ends_with(".emf") {
        Some("image/x-emf".to_owned())
    } else if lower.ends_with(".wmf") {
        Some("image/x-wmf".to_owned())
    } else {
        None
    }
}

fn extract_hyperlinks(
    node: Node<'_, '_>,
    source: &DocumentSource,
    rels: &BTreeMap<String, (String, String)>,
    link_counter: &mut usize,
    links: &mut Vec<Hyperlink>,
    paragraph_path: &str,
) {
    for link in node
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "hyperlink")))
    {
        let r_id = link
            .attribute((R_NS, "id"))
            .or_else(|| link.attribute("r:id"));
        let anchor = link
            .attribute((W_NS, "anchor"))
            .or_else(|| link.attribute("w:anchor"));
        let target = if let Some(r_id) = r_id {
            rels.get(r_id).map(|(_, t)| t.clone())
        } else {
            anchor.map(|a| format!("#{a}"))
        };

        if let Some(target) = target {
            let text = paragraph_text(link);
            if !text.trim().is_empty() {
                *link_counter += 1;
                let source_path = format!("/word/document.xml::link[{link_counter}]");
                links.push(Hyperlink {
                    id: source.object_id("lnk", &source_path),
                    text,
                    is_external: !target.starts_with('#'),
                    target,
                    page: None,
                    anchor_path: Some(paragraph_path.to_owned()),
                    source: SourceSpan::new(source_path),
                });
            }
        }
    }
}

fn parse_notes(
    xml_opt: Option<&str>,
    source: &DocumentSource,
    tag_name: &str,
    note_kind: NoteKind,
    reading_order: &mut u32,
    note_anchors: &BTreeMap<String, String>,
) -> Result<Vec<Block>, DocsightError> {
    let mut blocks = Vec::new();
    let Some(xml) = xml_opt else {
        return Ok(blocks);
    };
    let doc = XmlDocument::parse(xml).map_err(xml_error)?;
    for note in doc
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, tag_name)))
    {
        let Some(id) = note
            .attribute((W_NS, "id"))
            .or_else(|| note.attribute("w:id"))
        else {
            continue;
        };
        if id == "-1" || id == "0" {
            continue;
        }
        let text = note
            .children()
            .filter(|n| n.has_tag_name((W_NS, "p")))
            .map(paragraph_text)
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            continue;
        }
        *reading_order += 1;
        let prefix = match note_kind {
            NoteKind::Footnote => "fn",
            NoteKind::Endnote => "en",
        };
        let source_path = format!("/word/{tag_name}s.xml::{tag_name}[{id}]");
        let anchor_path = note_anchors.get(id).cloned();
        blocks.push(Block {
            id: source.object_id(prefix, &source_path),
            kind: BlockKind::Note,
            page: None,
            bbox: None,
            z_index: 0,
            reading_order: *reading_order,
            source: SourceSpan::new(source_path),
            confidence: 1.0,
            flags: LayoutFlags::default(),
            content: BlockContent::Note(NoteBlock {
                kind: note_kind,
                note_id: id.to_owned(),
                text,
                anchor_path,
            }),
        });
    }
    Ok(blocks)
}

fn parse_comments(
    xml_opt: Option<&str>,
    source: &DocumentSource,
    comment_anchors: &BTreeMap<String, String>,
) -> Result<Vec<Comment>, DocsightError> {
    let mut comments = Vec::new();
    let Some(xml) = xml_opt else {
        return Ok(comments);
    };
    let doc = XmlDocument::parse(xml).map_err(xml_error)?;
    for comment in doc
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "comment")))
    {
        let Some(id) = comment
            .attribute((W_NS, "id"))
            .or_else(|| comment.attribute("w:id"))
        else {
            continue;
        };
        let author = comment
            .attribute((W_NS, "author"))
            .or_else(|| comment.attribute("w:author"))
            .map(str::to_owned);
        let date = comment
            .attribute((W_NS, "date"))
            .or_else(|| comment.attribute("w:date"))
            .map(str::to_owned);
        let text = comment
            .children()
            .filter(|n| n.has_tag_name((W_NS, "p")))
            .map(paragraph_text)
            .collect::<Vec<_>>()
            .join("\n");
        let source_path = format!("/word/comments.xml::comment[{id}]");
        comments.push(Comment {
            id: source.object_id("cmt", &source_path),
            author,
            date,
            text,
            page: None,
            anchor_path: comment_anchors.get(id).cloned(),
            source: SourceSpan::new(source_path),
        });
    }
    Ok(comments)
}

fn parse_unknown_body_element(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    reading_order: u32,
    warnings: &mut Vec<Diagnostic>,
) -> Block {
    let raw_tag = node.tag_name().name().to_owned();
    let mut child_names: BTreeSet<String> = BTreeSet::new();
    for child in node.children().filter(Node::is_element) {
        child_names.insert(child.tag_name().name().to_owned());
    }
    let details = if child_names.is_empty() {
        None
    } else {
        Some(child_names.into_iter().collect::<Vec<_>>().join(", "))
    };
    let source_path = format!("/word/document.xml::body/{raw_tag}[{index}]");
    let id = source.object_id("unk", &source_path);
    warnings.push(Diagnostic {
        code: "DOCX_BODY_ELEMENT_UNSUPPORTED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "body element {raw_tag} was not interpreted and is preserved as an opaque node"
        ),
        effect: "the element content is retained without semantic interpretation".to_owned(),
        object: Some(id.clone()),
        page: None,
    });
    Block {
        id,
        kind: BlockKind::Unknown,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order,
        source: SourceSpan::new(source_path),
        confidence: 1.0,
        flags: LayoutFlags::default(),
        content: BlockContent::Unknown(UnknownBlock { raw_tag, details }),
    }
}

const KNOWN_PARAGRAPH_CHILDREN: &[&str] = &[
    "pPr",
    "r",
    "hyperlink",
    "bookmarkStart",
    "bookmarkEnd",
    "proofErr",
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "commentRangeStart",
    "commentRangeEnd",
    "commentReference",
    "fldSimple",
    "sdt",
    "smartTag",
    "oMath",
    "oMathPara",
    "customXml",
];

const KNOWN_RUN_CHILDREN: &[&str] = &[
    "rPr",
    "t",
    "tab",
    "br",
    "cr",
    "drawing",
    "pict",
    "footnoteReference",
    "endnoteReference",
    "commentReference",
    "fldChar",
    "instrText",
    "delText",
    "noBreakHyphen",
    "softHyphen",
];

const RUN_CONTAINERS: &[&str] = &[
    "r",
    "hyperlink",
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "fldSimple",
    "smartTag",
];

fn unsupported_run_elements(node: Node<'_, '_>) -> Vec<String> {
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for child in node.children().filter(Node::is_element) {
        let name = child.tag_name().name();
        if RUN_CONTAINERS.contains(&name) {
            for grandchild in child.children().filter(Node::is_element) {
                let inner = grandchild.tag_name().name();
                if !KNOWN_RUN_CHILDREN.contains(&inner) {
                    unknown.insert(format!("{name}/{inner}"));
                }
            }
        } else if !KNOWN_PARAGRAPH_CHILDREN.contains(&name) {
            unknown.insert(format!("p/{name}"));
        }
    }
    if node
        .descendants()
        .any(|descendant| descendant.tag_name().name() == "oMath")
    {
        unknown.insert("oMath".to_owned());
    }
    unknown.into_iter().collect()
}

struct ParagraphSegments {
    texts: Vec<String>,
    leading_page_break: bool,
    trailing_page_break: bool,
}

fn paragraph_segments(node: Node<'_, '_>) -> ParagraphSegments {
    let mut texts = vec![String::new()];
    let mut leading_page_break = false;
    for descendant in node.descendants().filter(Node::is_element) {
        if descendant
            .ancestors()
            .any(|ancestor| ancestor.has_tag_name((W_NS, "del")))
        {
            continue;
        }
        if descendant.ancestors().any(|ancestor| {
            ancestor != descendant
                && ancestor.tag_name().name() == "fldSimple"
                && ancestor.attributes().any(|attribute| {
                    (attribute.name() == "instr" || attribute.name().ends_with(":instr"))
                        && attribute.value().to_uppercase().contains("PAGE")
                })
        }) {
            continue;
        }
        if descendant.tag_name().name() == "fldSimple" {
            let is_page = descendant.attributes().any(|attribute| {
                (attribute.name() == "instr" || attribute.name().ends_with(":instr"))
                    && attribute.value().to_uppercase().contains("PAGE")
            });
            if is_page {
                if let Some(last) = texts.last_mut() {
                    last.push_str("[PAGE]");
                }
            }
            continue;
        }
        if descendant.has_tag_name((W_NS, "t")) {
            if let Some(value) = descendant.text() {
                if let Some(last) = texts.last_mut() {
                    last.push_str(value);
                }
            }
        } else if descendant.has_tag_name((W_NS, "tab")) {
            if let Some(last) = texts.last_mut() {
                last.push('\t');
            }
        } else if descendant.has_tag_name((W_NS, "br")) || descendant.has_tag_name((W_NS, "cr")) {
            let is_page_break = descendant.has_tag_name((W_NS, "br"))
                && descendant
                    .attribute((W_NS, "type"))
                    .or_else(|| descendant.attribute("w:type"))
                    .is_some_and(|value| value == "page");
            if is_page_break {
                let first_segment_empty = texts.len() == 1 && texts[0].trim().is_empty();
                if first_segment_empty {
                    leading_page_break = true;
                } else {
                    texts.push(String::new());
                }
            } else if let Some(last) = texts.last_mut() {
                last.push('\n');
            }
        }
    }
    let mut trailing_page_break = false;
    if texts.len() > 1 && texts.last().is_some_and(|text| text.trim().is_empty()) {
        texts.pop();
        trailing_page_break = true;
    }
    ParagraphSegments {
        texts,
        leading_page_break,
        trailing_page_break,
    }
}

fn on_off_value(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(value) => !matches!(value, "0" | "false" | "off"),
    }
}

fn paragraph_layout_flags(node: Node<'_, '_>) -> LayoutFlags {
    let mut flags = LayoutFlags::default();
    let Some(properties) = child_element(node, "pPr") else {
        return flags;
    };
    flags.page_break_before = child_element(properties, "pageBreakBefore")
        .is_some_and(|flag| on_off_value(word_value(flag).as_deref()));
    flags.keep_with_next = child_element(properties, "keepNext")
        .is_some_and(|flag| on_off_value(word_value(flag).as_deref()));
    flags.keep_lines = child_element(properties, "keepLines")
        .is_some_and(|flag| on_off_value(word_value(flag).as_deref()));
    flags
}

fn parse_paragraph(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    styles: &BTreeMap<String, StyleDefinition>,
    numbering: &NumberingDefinitions,
    reading_order: &mut u32,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Vec<Block>, DocsightError> {
    let base_path = format!("/word/document.xml::body/p[{index}]");
    let style_id = paragraph_property(node, "pStyle").and_then(|property| word_value(property));
    let direct_outline = child_element(node, "pPr")
        .map(outline_level)
        .transpose()?
        .flatten();
    let heading_level = match direct_outline {
        Some(level) => Some(level),
        None => resolve_heading_level(style_id.as_deref(), styles)?,
    };
    let direct_numbering = paragraph_numbering_properties(node)?;
    let inherited_numbering = resolve_style_numbering(style_id.as_deref(), styles)?;
    let list_reference = merge_numbering(direct_numbering, inherited_numbering);
    let list = resolve_list_marker(list_reference, numbering, warnings);
    let segments = paragraph_segments(node);
    let paragraph_flags = paragraph_layout_flags(node);
    let segment_count = segments.texts.len();
    let last_segment = segment_count.saturating_sub(1);

    let mut blocks = Vec::with_capacity(segment_count);
    for (segment_index, text) in segments.texts.into_iter().enumerate() {
        *reading_order = reading_order.checked_add(1).ok_or_else(block_count_error)?;
        let source_path = if segment_index == 0 {
            base_path.clone()
        } else {
            let part = segment_index + 1;
            format!("{base_path}::part[{part}]")
        };
        let mut flags = paragraph_flags;
        if segment_index > 0 || segments.leading_page_break {
            flags.page_break_before = true;
        }
        if segment_index == last_segment && segments.trailing_page_break {
            flags.break_after = true;
        }

        let (kind, prefix, content) = if let Some(level) = heading_level {
            (
                BlockKind::Heading,
                "h",
                BlockContent::Heading(HeadingBlock {
                    level,
                    text,
                    style_id: style_id.clone(),
                }),
            )
        } else if let Some(list) = &list {
            (
                BlockKind::ListItem,
                "li",
                BlockContent::ListItem(ListItemBlock {
                    level: list.level,
                    marker: list.pattern.clone(),
                    format: list.format.clone(),
                    pattern: list.pattern.clone(),
                    ordered: list.ordered,
                    text,
                    style_id: style_id.clone(),
                }),
            )
        } else {
            (
                BlockKind::Paragraph,
                "p",
                BlockContent::Paragraph(ParagraphBlock {
                    text,
                    style_id: style_id.clone(),
                }),
            )
        };

        let id = source.object_id(prefix, &source_path);
        if segment_index == 0 {
            warn_unsupported_run_content(node, &id, warnings);
        }

        blocks.push(Block {
            id,
            kind,
            page: None,
            bbox: None,
            z_index: 0,
            reading_order: *reading_order,
            source: SourceSpan::new(source_path),
            confidence: 1.0,
            flags,
            content,
        });
    }
    Ok(blocks)
}

fn warn_unsupported_run_content(
    node: Node<'_, '_>,
    block_id: &ObjectId,
    warnings: &mut Vec<Diagnostic>,
) {
    let unknown = unsupported_run_elements(node);
    if !unknown.is_empty() {
        warnings.push(Diagnostic {
            code: "DOCX_RUN_ELEMENT_UNSUPPORTED".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "paragraph contains run content that was not interpreted: {}",
                unknown.join(", ")
            ),
            effect: "the paragraph text may be incomplete because run content was not extracted"
                .to_owned(),
            object: Some(block_id.clone()),
            page: None,
        });
    }
}

fn parse_table(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    reading_order: u32,
) -> Result<Block, DocsightError> {
    let source_path = format!("/word/document.xml::body/tbl[{index}]");
    let (id, table_block, span) = parse_table_at(node, source_path, source, 0)?;
    Ok(Block {
        id,
        kind: BlockKind::Table,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order,
        source: span,
        confidence: 1.0,
        flags: LayoutFlags::default(),
        content: BlockContent::Table(table_block),
    })
}

fn parse_table_at(
    node: Node<'_, '_>,
    source_path: String,
    source: &DocumentSource,
    depth: usize,
) -> Result<(ObjectId, TableBlock, SourceSpan), DocsightError> {
    if depth > MAX_TABLE_DEPTH {
        return Err(DocsightError::ResourceLimit {
            resource: "nested table depth".to_owned(),
            limit: MAX_TABLE_DEPTH as u64,
        });
    }
    let mut cells: Vec<TableCell> = Vec::new();
    let mut active_merges: BTreeMap<u32, usize> = BTreeMap::new();
    let mut rows = 0_u32;
    let mut columns = 0_u32;
    let mut header_rows = 0_u32;
    for (row_offset, row_node) in node
        .children()
        .filter(|child| child.has_tag_name((W_NS, "tr")))
        .enumerate()
    {
        let row = u32::try_from(row_offset).map_err(|_| table_size_error())?;
        let row_number = row.checked_add(1).ok_or_else(table_size_error)?;
        rows = row_number;
        let mut column = 0_u32;
        let mut next_merges = BTreeMap::new();
        let mut extended = BTreeSet::new();
        let mut row_is_header = false;
        for cell_node in row_node
            .children()
            .filter(|child| child.has_tag_name((W_NS, "tc")))
        {
            row_is_header |= table_cell_is_header(cell_node);
            let column_span = table_cell_span(cell_node)?;
            let column_end = column
                .checked_add(column_span)
                .ok_or_else(table_size_error)?;
            let merge = table_cell_vertical_merge(cell_node)?;
            let column_number = column.checked_add(1).ok_or_else(table_size_error)?;
            let cell_source = format!("{source_path}/tr[{row_number}]/tc[{column_number}]");
            let text = table_cell_text(cell_node);
            let nested_tables = cell_node
                .children()
                .filter(|child| child.has_tag_name((W_NS, "tbl")))
                .enumerate()
                .map(|(nested_index, nested)| {
                    let nested_number = nested_index.checked_add(1).ok_or_else(table_size_error)?;
                    let nested_source = format!("{cell_source}/tbl[{nested_number}]");
                    let (nested_id, nested_table, nested_span) =
                        parse_table_at(nested, nested_source, source, depth + 1)?;
                    Ok(Block {
                        id: nested_id,
                        kind: BlockKind::Table,
                        page: None,
                        bbox: None,
                        z_index: 0,
                        reading_order: 0,
                        source: nested_span,
                        confidence: 1.0,
                        flags: LayoutFlags::default(),
                        content: BlockContent::Table(nested_table),
                    })
                })
                .collect::<Result<Vec<_>, DocsightError>>()?;
            match merge.as_deref() {
                Some("continue") => {
                    let cell_index = active_merges.get(&column).copied().ok_or_else(|| {
                        DocsightError::MalformedDocument {
                            message: format!("vertical merge has no restart at {cell_source}"),
                        }
                    })?;
                    if extended.insert(cell_index) {
                        cells[cell_index].row_span = cells[cell_index]
                            .row_span
                            .checked_add(1)
                            .ok_or_else(table_size_error)?;
                        if !text.is_empty() {
                            if !cells[cell_index].text.is_empty() {
                                cells[cell_index].text.push('\n');
                            }
                            cells[cell_index].text.push_str(&text);
                        }
                        cells[cell_index].blocks.extend(nested_tables);
                    }
                    for merged_column in column..column_end {
                        next_merges.insert(merged_column, cell_index);
                    }
                }
                Some("restart") | None => {
                    let cell_index = cells.len();
                    cells.push(TableCell {
                        id: source.object_id("cell", &cell_source),
                        row,
                        column,
                        row_span: 1,
                        column_span,
                        bbox: None,
                        text,
                        blocks: nested_tables,
                        source: SourceSpan::new(cell_source),
                    });
                    if merge.as_deref() == Some("restart") {
                        for merged_column in column..column_end {
                            next_merges.insert(merged_column, cell_index);
                        }
                    }
                }
                Some(value) => {
                    return Err(DocsightError::MalformedDocument {
                        message: format!("invalid vertical merge value at {cell_source}: {value}"),
                    });
                }
            }
            column = column_end;
        }
        if row_is_header {
            header_rows = header_rows.checked_add(1).ok_or_else(table_size_error)?;
        }
        columns = columns.max(column);
        active_merges = next_merges;
    }
    let column_widths_pt = table_grid_widths(node);
    let id = source.object_id("tbl", &source_path);
    let span = SourceSpan::new(source_path);
    Ok((
        id,
        TableBlock {
            rows,
            columns,
            header_rows,
            cells,
            column_widths_pt,
        },
        span,
    ))
}

fn table_grid_widths(node: Node<'_, '_>) -> Option<Vec<f32>> {
    let grid = child_element(node, "tblGrid")?;
    let widths: Vec<f32> = grid
        .children()
        .filter(|child| child.has_tag_name((W_NS, "gridCol")))
        .filter_map(|column| {
            column
                .attribute((W_NS, "w"))
                .and_then(|value| value.parse::<f32>().ok())
                .map(|dxa| dxa / 20.0)
        })
        .collect();
    (!widths.is_empty()).then_some(widths)
}

fn table_cell_is_header(node: Node<'_, '_>) -> bool {
    child_element(node, "tcPr")
        .map(|properties| child_element(properties, "tblHeader").is_some())
        .unwrap_or(false)
}

fn paragraph_text(node: Node<'_, '_>) -> String {
    let mut text = String::new();
    for descendant in node.descendants().filter(Node::is_element) {
        if descendant
            .ancestors()
            .any(|ancestor| ancestor.has_tag_name((W_NS, "del")))
        {
            continue;
        }
        if descendant.ancestors().any(|ancestor| {
            ancestor != descendant
                && ancestor.tag_name().name() == "fldSimple"
                && ancestor.attributes().any(|a| {
                    (a.name() == "instr" || a.name().ends_with(":instr"))
                        && a.value().to_uppercase().contains("PAGE")
                })
        }) {
            continue;
        }
        if descendant.tag_name().name() == "fldSimple" {
            let is_page = descendant.attributes().any(|a| {
                (a.name() == "instr" || a.name().ends_with(":instr"))
                    && a.value().to_uppercase().contains("PAGE")
            });
            if is_page {
                text.push_str("[PAGE]");
                continue;
            }
        }
        if descendant.has_tag_name((W_NS, "t")) {
            if let Some(value) = descendant.text() {
                text.push_str(value);
            }
        } else if descendant.has_tag_name((W_NS, "tab")) {
            text.push('\t');
        } else if descendant.has_tag_name((W_NS, "br")) || descendant.has_tag_name((W_NS, "cr")) {
            text.push('\n');
        }
    }
    text
}

fn table_cell_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|child| child.has_tag_name((W_NS, "p")))
        .map(paragraph_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn table_cell_span(node: Node<'_, '_>) -> Result<u32, DocsightError> {
    let span = child_element(node, "tcPr")
        .and_then(|properties| child_element(properties, "gridSpan"))
        .and_then(word_value)
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("invalid table grid span: {error}"),
        })?
        .unwrap_or(1);
    if span == 0 {
        return Err(DocsightError::MalformedDocument {
            message: "table grid span must be greater than zero".to_owned(),
        });
    }
    Ok(span)
}

fn table_cell_vertical_merge(node: Node<'_, '_>) -> Result<Option<String>, DocsightError> {
    let Some(merge) =
        child_element(node, "tcPr").and_then(|properties| child_element(properties, "vMerge"))
    else {
        return Ok(None);
    };
    Ok(Some(
        word_value(merge).unwrap_or_else(|| "continue".to_owned()),
    ))
}

fn parse_styles(xml: Option<&str>) -> Result<BTreeMap<String, StyleDefinition>, DocsightError> {
    let Some(xml) = xml else {
        return Ok(BTreeMap::new());
    };
    let document = XmlDocument::parse(xml).map_err(xml_error)?;
    let mut styles = BTreeMap::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "style")))
    {
        if node.attribute((W_NS, "type")) != Some("paragraph") {
            continue;
        }
        let style_id =
            node.attribute((W_NS, "styleId"))
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "paragraph style has no styleId".to_owned(),
                })?;
        let name = child_element(node, "name").and_then(word_value);
        let based_on = child_element(node, "basedOn").and_then(word_value);
        let outline_level = child_element(node, "pPr")
            .map(outline_level)
            .transpose()?
            .flatten();
        let numbering = style_numbering_properties(node)?;
        styles.insert(
            style_id.to_owned(),
            StyleDefinition {
                name,
                based_on,
                outline_level,
                numbering,
            },
        );
    }
    Ok(styles)
}

fn heading_level_from_label(label: &str) -> Option<u8> {
    let normalized: String = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let suffix = normalized.strip_prefix("heading")?;
    let level = suffix.parse::<u8>().ok()?;
    (1..=9).contains(&level).then_some(level)
}

fn resolve_heading_level(
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
) -> Result<Option<u8>, DocsightError> {
    let Some(mut current) = style_id else {
        return Ok(None);
    };
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_STYLE_DEPTH {
        if !visited.insert(current.to_owned()) {
            return Err(DocsightError::MalformedDocument {
                message: format!("cycle detected in paragraph style inheritance: {current}"),
            });
        }
        let Some(style) = styles.get(current) else {
            return Ok(heading_level_from_label(current));
        };
        if let Some(level) = style.outline_level {
            return Ok(Some(level));
        }
        if let Some(level) = style.name.as_deref().and_then(heading_level_from_label) {
            return Ok(Some(level));
        }
        if let Some(level) = heading_level_from_label(current) {
            return Ok(Some(level));
        }
        let Some(parent) = style.based_on.as_deref() else {
            return Ok(None);
        };
        current = parent;
    }
    Err(DocsightError::MalformedDocument {
        message: format!("paragraph style inheritance exceeds {MAX_STYLE_DEPTH} levels"),
    })
}

fn resolve_style_numbering(
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
) -> Result<Option<NumberingProperties>, DocsightError> {
    let mut current = style_id;
    let mut depth = 0_usize;
    let mut visited = BTreeSet::new();
    while let Some(id) = current {
        if !visited.insert(id) {
            return Err(DocsightError::MalformedDocument {
                message: format!("cycle detected in style inheritance: {id}"),
            });
        }
        depth = depth.checked_add(1).ok_or_else(block_count_error)?;
        if depth > MAX_STYLE_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "style inheritance depth".to_owned(),
                limit: MAX_STYLE_DEPTH as u64,
            });
        }
        let Some(definition) = styles.get(id) else {
            return Ok(None);
        };
        if definition.numbering.is_some() {
            return Ok(definition.numbering.clone());
        }
        current = definition.based_on.as_deref();
    }
    Ok(None)
}

fn parse_numbering(xml: Option<&str>) -> Result<NumberingDefinitions, DocsightError> {
    let Some(xml) = xml else {
        return Ok(NumberingDefinitions::default());
    };
    let document = XmlDocument::parse(xml).map_err(xml_error)?;
    let mut abstract_numbers = BTreeMap::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "abstractNum")))
    {
        let abstract_id = node.attribute((W_NS, "abstractNumId")).ok_or_else(|| {
            DocsightError::MalformedDocument {
                message: "abstractNum has no abstractNumId".to_owned(),
            }
        })?;
        let mut levels = BTreeMap::new();
        for level_node in node
            .children()
            .filter(|child| child.has_tag_name((W_NS, "lvl")))
        {
            let level = level_node
                .attribute((W_NS, "ilvl"))
                .and_then(|value| value.parse::<u8>().ok())
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "numbering level has no ilvl".to_owned(),
                })?;
            let format = child_element(level_node, "numFmt").and_then(word_value);
            let pattern = child_element(level_node, "lvlText").and_then(word_value);
            levels.insert(level, NumberingLevel { format, pattern });
        }
        abstract_numbers.insert(abstract_id.to_owned(), levels);
    }
    let mut numbers = BTreeMap::new();
    let mut levels = BTreeMap::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "num")))
    {
        let num_id =
            node.attribute((W_NS, "numId"))
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "num has no numId".to_owned(),
                })?;
        let Some(abstract_id) = child_element(node, "abstractNumId").and_then(word_value) else {
            continue;
        };
        numbers.insert(num_id.to_owned(), abstract_id.clone());
        if let Some(abstract_levels) = abstract_numbers.get(&abstract_id) {
            for (level, definition) in abstract_levels {
                levels.insert((abstract_id.clone(), *level), definition.clone());
            }
        }
    }
    Ok(NumberingDefinitions { numbers, levels })
}

fn resolve_list_marker(
    reference: Option<(String, u8)>,
    numbering: &NumberingDefinitions,
    warnings: &mut Vec<Diagnostic>,
) -> Option<ListMarker> {
    let (num_id, level) = reference?;
    let definition = numbering
        .numbers
        .get(&num_id)
        .and_then(|abstract_id| numbering.levels.get(&(abstract_id.clone(), level)));
    let (format, pattern, ordered) = match definition {
        Some(definition) => {
            if definition.format.is_none() {
                warnings.push(Diagnostic::warning(
                    "DOCX_NUMBERING_FORMAT_MISSING",
                    format!("numbering format is missing for numId {num_id}"),
                    "the list item ordering semantics are unavailable",
                ));
            }
            let ordered = definition
                .format
                .as_deref()
                .map(|value| value != "bullet" && value != "none");
            (
                definition.format.clone(),
                definition.pattern.clone(),
                ordered,
            )
        }
        None => {
            warnings.push(Diagnostic::warning(
                "DOCX_NUMBERING_UNRESOLVED",
                format!("numbering definition was not resolved for numId {num_id}"),
                "the list item marker format is unknown",
            ));
            (None, None, None)
        }
    };
    Some(ListMarker {
        level,
        format,
        pattern,
        ordered,
    })
}

fn paragraph_numbering_properties(
    node: Node<'_, '_>,
) -> Result<Option<NumberingProperties>, DocsightError> {
    let Some(properties) = child_element(node, "pPr") else {
        return Ok(None);
    };
    numbering_properties(properties)
}

fn style_numbering_properties(
    node: Node<'_, '_>,
) -> Result<Option<NumberingProperties>, DocsightError> {
    let Some(properties) = child_element(node, "pPr") else {
        return Ok(None);
    };
    numbering_properties(properties)
}

fn numbering_properties(
    properties: Node<'_, '_>,
) -> Result<Option<NumberingProperties>, DocsightError> {
    let Some(num_properties) = child_element(properties, "numPr") else {
        return Ok(None);
    };
    let num_id = child_element(num_properties, "numId").and_then(word_value);
    let level = child_element(num_properties, "ilvl")
        .and_then(word_value)
        .map(|value| value.parse::<u8>())
        .transpose()
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("invalid numbering level: {error}"),
        })?;
    Ok(Some(NumberingProperties { num_id, level }))
}

fn merge_numbering(
    direct: Option<NumberingProperties>,
    inherited: Option<NumberingProperties>,
) -> Option<(String, u8)> {
    let direct = direct.unwrap_or_default();
    let inherited = inherited.unwrap_or_default();
    let num_id = direct.num_id.or(inherited.num_id)?;
    let level = direct.level.or(inherited.level).unwrap_or(0);
    Some((num_id, level))
}

fn paragraph_property<'a>(node: Node<'a, 'a>, property: &str) -> Option<Node<'a, 'a>> {
    child_element(node, "pPr").and_then(|properties| child_element(properties, property))
}

fn child_element<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|child| child.has_tag_name((W_NS, name)))
}

fn word_value(node: Node<'_, '_>) -> Option<String> {
    node.attribute((W_NS, "val")).map(str::to_owned)
}

fn outline_level(properties: Node<'_, '_>) -> Result<Option<u8>, DocsightError> {
    let Some(property) = child_element(properties, "outlineLvl") else {
        return Ok(None);
    };
    let value = word_value(property).ok_or_else(|| DocsightError::MalformedDocument {
        message: "outline level has no value".to_owned(),
    })?;
    let zero_based = value
        .parse::<u8>()
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("invalid outline level: {error}"),
        })?;
    if zero_based == 9 {
        return Ok(None);
    }
    if zero_based > 9 {
        return Err(DocsightError::MalformedDocument {
            message: format!("outline level exceeds the OOXML range: {zero_based}"),
        });
    }
    let level = zero_based.checked_add(1).ok_or_else(block_count_error)?;
    Ok(Some(level))
}

fn xml_error(error: roxmltree::Error) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("invalid OOXML: {error}"),
    }
}

fn table_size_error() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "table dimensions".to_owned(),
        limit: u32::MAX as u64,
    }
}

fn block_count_error() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "DOCX block count".to_owned(),
        limit: u32::MAX as u64,
    }
}
