use crate::package::read_parts;
use crate::sections::{
    SectionInputs, SectionLocation, defaulted_section, even_and_odd_headers, header_footer_parts,
    parse_section,
};
use docsight_core::{
    Block, BlockContent, BlockKind, Comment, Diagnostic, DiagnosticSeverity, DocsightError,
    Document, DocumentFormat, DocumentMetadata, DocumentSource, FigureBlock, HeadingBlock,
    Hyperlink, InlineRun, IrVersion, LayoutFlags, ListItemBlock, NoteBlock, NoteKind, ObjectId,
    ParagraphBlock, ParagraphFormat, Resource, ResourceKind, RunFormat, Section, ShapeBlock,
    SourceSpan, Style, TableBlock, TableCell, TextAlignment, TrackedChanges, UnknownBlock,
    validate_canonical,
};
use roxmltree::{Document as XmlDocument, Node, ParsingOptions};
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";
const EXTENDED_PROPERTIES_NS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_STYLE_DEPTH: usize = 64;
const MAX_TABLE_DEPTH: usize = 32;
pub const MAX_XML_DEPTH: usize = 64;
pub const MAX_XML_NODES: u32 = 65_536;
pub const MAX_XML_TOKEN_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default)]
struct StyleDefinition {
    name: Option<String>,
    based_on: Option<String>,
    paragraph_format: ParagraphFormat,
    outline_level: Option<u8>,
    numbering: Option<NumberingProperties>,
    font_family: Option<String>,
    font_size_pt: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    color_argb: Option<u32>,
    underline: Option<bool>,
    layout_flags: StyleLayoutFlags,
    cell_format: CellFormatPatch,
}

#[derive(Clone, Debug, Default)]
struct CellFormatPatch {
    padding: [Option<f32>; 4],
    fill: Option<u32>,
    borders: [Option<Option<docsight_core::CellBorder>>; 6],
}

fn cell_format_patch(properties: Node<'_, '_>) -> Result<CellFormatPatch, DocsightError> {
    let mut patch = CellFormatPatch::default();
    if let Some(margins) =
        child_element(properties, "tcMar").or_else(|| child_element(properties, "tblCellMar"))
    {
        for (index, names) in [
            (0, ["top", "top"]),
            (1, ["end", "right"]),
            (2, ["bottom", "bottom"]),
            (3, ["start", "left"]),
        ] {
            if let Some(margin) =
                child_element(margins, names[0]).or_else(|| child_element(margins, names[1]))
            {
                patch.padding[index] = attribute_points(margin, &["w"], "table cell margin")?;
            }
        }
    }
    if let Some(shading) =
        child_element(properties, "shd").and_then(|node| node.attribute((W_NS, "fill")))
        && shading != "auto"
    {
        patch.fill = Some(parse_argb(shading)?);
    }
    if let Some(borders) =
        child_element(properties, "tcBorders").or_else(|| child_element(properties, "tblBorders"))
    {
        for (index, name) in ["top", "right", "bottom", "left", "insideH", "insideV"]
            .into_iter()
            .enumerate()
        {
            if let Some(border) = child_element(borders, name) {
                let style = word_value(border).unwrap_or_else(|| "single".to_owned());
                patch.borders[index] = Some(match style.as_str() {
                    "nil" | "none" => None,
                    "single" => {
                        let raw = border
                            .attribute((W_NS, "sz"))
                            .unwrap_or("4")
                            .parse::<u32>()
                            .map_err(|_| DocsightError::MalformedDocument {
                                message: "table border size must be an unsigned integer".to_owned(),
                            })?;
                        let color = border
                            .attribute((W_NS, "color"))
                            .filter(|color| *color != "auto")
                            .map(parse_argb)
                            .transpose()?
                            .unwrap_or(0xff000000);
                        Some(docsight_core::CellBorder {
                            width_pt: raw as f32 / 8.0,
                            color_argb: color,
                        })
                    }
                    _ => {
                        return Err(DocsightError::UnsupportedFeature {
                            feature: format!("DOCX table border style {style}"),
                        });
                    }
                });
            }
        }
    }
    Ok(patch)
}

fn parse_argb(value: &str) -> Result<u32, DocsightError> {
    if value.len() != 6 {
        return Err(DocsightError::MalformedDocument {
            message: format!("invalid RGB color {value}"),
        });
    }
    u32::from_str_radix(value, 16)
        .map(|color| color | 0xff000000)
        .map_err(|_| DocsightError::MalformedDocument {
            message: format!("invalid RGB color {value}"),
        })
}

fn apply_cell_patch(
    format: &mut docsight_core::CellFormat,
    patch: &CellFormatPatch,
    row: u32,
    column: u32,
    rows: u32,
    columns: u32,
) {
    for index in 0..4 {
        if let Some(padding) = patch.padding[index] {
            format.padding_pt[index] = padding;
        }
        let inner = match index {
            0 if row > 0 => Some(4),
            1 if column + 1 < columns => Some(5),
            2 if row + 1 < rows => Some(4),
            3 if column > 0 => Some(5),
            _ => None,
        };
        if let Some(border) = inner
            .and_then(|inner| patch.borders[inner])
            .or(patch.borders[index])
        {
            format.borders[index] = border;
        }
    }
    format.fill_argb = patch.fill.or(format.fill_argb);
}

#[derive(Clone, Copy, Debug, Default)]
struct StyleLayoutFlags {
    page_break_before: Option<bool>,
    keep_with_next: Option<bool>,
    keep_lines: Option<bool>,
    widow_control: Option<bool>,
}

impl StyleLayoutFlags {
    fn from_properties(properties: Node<'_, '_>) -> Self {
        let flag = |name: &str| {
            child_element(properties, name).map(|flag| on_off_value(word_value(flag).as_deref()))
        };
        Self {
            page_break_before: flag("pageBreakBefore"),
            keep_with_next: flag("keepNext"),
            keep_lines: flag("keepLines"),
            widow_control: flag("widowControl"),
        }
    }

    fn merge(&mut self, source: Self) {
        self.page_break_before = source.page_break_before.or(self.page_break_before);
        self.keep_with_next = source.keep_with_next.or(self.keep_with_next);
        self.keep_lines = source.keep_lines.or(self.keep_lines);
        self.widow_control = source.widow_control.or(self.widow_control);
    }

    fn resolve(self) -> LayoutFlags {
        LayoutFlags {
            page_break_before: self.page_break_before.unwrap_or(false),
            break_after: false,
            keep_with_next: self.keep_with_next.unwrap_or(false),
            keep_lines: self.keep_lines.unwrap_or(false),
            widow_control: self.widow_control.unwrap_or(true),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ParagraphDefaults {
    style_id: Option<String>,
    layout_flags: StyleLayoutFlags,
    run_format: RunFormat,
    paragraph_format: ParagraphFormat,
}

#[derive(Clone, Debug, Default)]
struct ResolvedStyleFormatting {
    font_family: Option<String>,
    font_size_pt: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    color_argb: Option<u32>,
    underline: Option<bool>,
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
    start: u32,
    format_properties: RunFormat,
    paragraph_format: ParagraphFormat,
}

#[derive(Clone, Debug, Default)]
struct NumberingDefinitions {
    numbers: BTreeMap<String, String>,
    levels: BTreeMap<(String, u8), NumberingLevel>,
}

#[derive(Clone, Debug)]
struct ListMarker {
    numbering_id: String,
    format_properties: Option<RunFormat>,
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
    let header_footer_contents = header_footer_parts(&parts.headers, &parts.footers)?;
    let section_inputs = SectionInputs {
        source,
        rels: &rels,
        parts: &header_footer_contents,
        even_and_odd_headers: even_and_odd_headers(parts.settings.as_deref())?,
    };
    let styles_document = parts.styles.as_deref().map(parse_xml).transpose()?;
    let styles = parse_styles(styles_document.as_ref())?;
    let paragraph_defaults = parse_paragraph_defaults(styles_document.as_ref())?;
    let numbering = parse_numbering(parts.numbering.as_deref())?;
    let xml = parse_xml(&parts.document)?;
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
    preserve_inert_parts(
        source,
        &parts.inert_part_digests,
        &mut resources,
        &mut warnings,
    );
    let mut paragraph_index = 0_u32;
    let mut table_index = 0_u32;
    let mut body_section_index = 0_u32;
    let mut section_start_block = 0_usize;
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
                &paragraph_defaults,
                &numbering,
                &mut reading_order,
                &mut warnings,
            )?;
            blocks.extend(paragraph_blocks);
            let child_figures = extract_figures(
                child,
                source,
                &rels,
                &mut reading_order,
                &mut figure_index,
                &mut resources,
                &parts.binary_part_digests,
                &mut warnings,
            )?;
            figure_warnings(
                child_figures.iter(),
                &resources,
                &parts.binary_part_formats,
                &mut warnings,
            );
            blocks.extend(child_figures);
            collect_anchor_ids(
                child,
                &paragraph_path,
                &mut note_anchors,
                &mut comment_anchors,
            );
            if let Some(section_node) = child_element(child, "pPr")
                .and_then(|properties| child_element(properties, "sectPr"))
            {
                let source_path = format!("{paragraph_path}/pPr/sectPr");
                push_section(
                    section_node,
                    &source_path,
                    &blocks,
                    &mut section_start_block,
                    &mut sections,
                    &section_inputs,
                    &mut warnings,
                )?;
            }
        } else if child.has_tag_name((W_NS, "tbl")) {
            table_index = table_index.checked_add(1).ok_or_else(block_count_error)?;
            reading_order = reading_order.checked_add(1).ok_or_else(block_count_error)?;
            blocks.push(parse_table(
                child,
                table_index,
                source,
                reading_order,
                &TableContext {
                    styles: &styles,
                    defaults: &paragraph_defaults,
                    numbering: &numbering,
                    header_tokens: None,
                },
            )?);
        } else if child.has_tag_name((W_NS, "sectPr")) {
            body_section_index = body_section_index
                .checked_add(1)
                .ok_or_else(block_count_error)?;
            let source_path = format!("/word/document.xml::body/sectPr[{body_section_index}]");
            push_section(
                child,
                &source_path,
                &blocks,
                &mut section_start_block,
                &mut sections,
                &section_inputs,
                &mut warnings,
            )?;
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

    let rounded_measures = rounded_twips_measures(xml.root())
        + styles_document
            .as_ref()
            .map_or(0, |styles| rounded_twips_measures(styles.root()));
    if rounded_measures > 0 {
        warnings.push(Diagnostic::warning(
            "DOCX_MEASURE_ROUNDED",
            format!(
                "{rounded_measures} length measures are written as decimals within {TWIPS_ROUNDING_TOLERANCE} of a whole number of twentieths of a point"
            ),
            "each measure was read as the nearest whole number of twentieths of a point",
        ));
    }

    if sections.is_empty() || section_start_block < blocks.len() {
        warnings.push(Diagnostic::warning(
            "DOCX_SECTION_DEFAULTED",
            "document.xml does not end with a sectPr; a default final section was synthesized"
                .to_owned(),
            "page geometry of the final section falls back to Letter 612x792 pt with 72 pt margins and no headers or footers",
        ));
        let index = next_section_index(&sections)?;
        let source_path = format!(
            "/word/document.xml::body/sectPr[{}]",
            body_section_index
                .checked_add(1)
                .ok_or_else(block_count_error)?
        );
        sections.push(defaulted_section(
            source,
            index,
            &source_path,
            section_last_block(&blocks, section_start_block),
            section_inputs.even_and_odd_headers,
        ));
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

    for section in &mut sections {
        for entry in &mut section.headers_footers {
            let raw = parts
                .headers
                .iter()
                .chain(&parts.footers)
                .find(|(name, _)| format!("/{name}") == entry.part)
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: format!("header or footer {} has no source part", entry.part),
                })?;
            entry.blocks =
                formatted_header_footer(&raw.1, &entry.part, source, &styles, &paragraph_defaults)?;
        }
    }
    realize_numbering(&mut blocks, &numbering, &mut BTreeMap::new())?;
    let converted_styles: Vec<Style> = styles
        .iter()
        .map(|(style_id, style_def)| {
            let formatting = resolve_style_formatting(style_id, &styles)?;
            Ok(Style {
                id: style_id.clone(),
                name: style_def.name.clone(),
                based_on: style_def.based_on.clone(),
                font_family: formatting.font_family,
                font_size_pt: formatting.font_size_pt,
                bold: formatting.bold,
                italic: formatting.italic,
            })
        })
        .collect::<Result<Vec<_>, DocsightError>>()?;

    let document = Document {
        version: IrVersion::current(),
        id: source.id(),
        sha256: source.sha256().to_owned(),
        format: DocumentFormat::Docx,
        size_bytes: source.size_bytes(),
        metadata: document_metadata(
            parts.core_properties.as_deref(),
            parts.extended_properties.as_deref(),
        )?,
        styles: converted_styles,
        sections,
        pages: Vec::new(),
        blocks,
        resources,
        links,
        comments,
        tracked_changes,
        warnings,
    };
    validate_canonical(&document)?;
    Ok(document)
}

fn document_metadata(
    core_xml: Option<&str>,
    extended_xml: Option<&str>,
) -> Result<DocumentMetadata, DocsightError> {
    let mut metadata = DocumentMetadata::default();
    if let Some(xml) = core_xml {
        let document = parse_xml(xml)?;
        let root = document.root_element();
        metadata.title = namespaced_text(root, DC_NS, "title");
        metadata.author = namespaced_text(root, DC_NS, "creator");
        metadata.subject = namespaced_text(root, DC_NS, "subject");
    }
    if let Some(xml) = extended_xml {
        let document = parse_xml(xml)?;
        metadata.producer = namespaced_text(
            document.root_element(),
            EXTENDED_PROPERTIES_NS,
            "Application",
        );
    }
    Ok(metadata)
}

fn namespaced_text(root: Node<'_, '_>, namespace: &str, name: &str) -> Option<String> {
    let text = root
        .children()
        .find(|child| child.has_tag_name((namespace, name)))?
        .text()?
        .trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn preserve_inert_parts(
    source: &DocumentSource,
    inert_part_digests: &BTreeMap<String, String>,
    resources: &mut Vec<Resource>,
    warnings: &mut Vec<Diagnostic>,
) {
    for (path, digest) in inert_part_digests {
        let id = source.object_id("res", path);
        let lower = path.to_ascii_lowercase();
        let code = if lower.starts_with("word/embeddings/") {
            "DOCX_EMBEDDED_OBJECT_INERT"
        } else {
            "DOCX_ACTIVE_CONTENT_INERT"
        };
        resources.push(Resource {
            id: id.clone(),
            kind: ResourceKind::EmbeddedObject,
            name: path.clone(),
            target: path.clone(),
            mime_type: None,
            content_sha256: Some(digest.clone()),
        });
        warnings.push(Diagnostic {
            code: code.to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!("embedded part {path} was preserved as inert content"),
            effect: "the content was not executed or semantically interpreted; only its digest is available"
                .to_owned(),
            object: Some(id),
            page: None,
            occurrences: None,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn push_section(
    node: Node<'_, '_>,
    source_path: &str,
    blocks: &[Block],
    section_start_block: &mut usize,
    sections: &mut Vec<Section>,
    inputs: &SectionInputs<'_>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(), DocsightError> {
    let location = SectionLocation {
        index: next_section_index(sections)?,
        source_path,
        last_block_id: section_last_block(blocks, *section_start_block),
    };
    let section = parse_section(node, location, sections.last(), inputs, warnings)?;
    sections.push(section);
    *section_start_block = blocks.len();
    Ok(())
}

fn next_section_index(sections: &[Section]) -> Result<u32, DocsightError> {
    u32::try_from(sections.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| DocsightError::ResourceLimit {
            resource: "DOCX section count".to_owned(),
            limit: u64::from(u32::MAX),
        })
}

fn section_last_block(blocks: &[Block], section_start_block: usize) -> Option<ObjectId> {
    blocks
        .get(section_start_block..)
        .and_then(<[Block]>::last)
        .map(|block| block.id.clone())
}

pub(crate) fn normalize_internal_target(target: &str) -> Option<String> {
    if target.contains("://") || target.contains('\\') || target.contains(':') {
        return None;
    }
    if target.starts_with('/') {
        let mut segments: Vec<&str> = Vec::new();
        for segment in target.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop()?;
                }
                other => segments.push(other),
            }
        }
        if segments.is_empty() {
            return None;
        }
        return Some(segments.join("/"));
    }
    let source_dir = "word";
    let mut segments: Vec<&str> = source_dir.split('/').collect();
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
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
    let doc = parse_xml(xml)?;
    for rel in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Relationship")
    {
        let id = rel
            .attribute("Id")
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: "relationship has no Id".to_owned(),
            })?;
        let target = rel
            .attribute("Target")
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: format!("relationship {id} has no Target"),
            })?;
        let rel_type = rel
            .attribute("Type")
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: format!("relationship {id} has no Type"),
            })?;
        if target.is_empty() {
            return Err(DocsightError::MalformedDocument {
                message: format!("relationship {id} has an empty Target"),
            });
        }
        let looks_external = target.contains("://") || target.starts_with("mailto:");
        match rel.attribute("TargetMode") {
            Some("External") => {}
            None if !looks_external && normalize_internal_target(target).is_some() => {}
            None if !looks_external => {
                return Err(DocsightError::MalformedDocument {
                    message: format!("relationship {id} has an invalid internal target"),
                });
            }
            None => {
                return Err(DocsightError::MalformedDocument {
                    message: format!("relationship {id} has an external target without TargetMode"),
                });
            }
            Some(mode) => {
                return Err(DocsightError::MalformedDocument {
                    message: format!("relationship {id} has unsupported TargetMode {mode}"),
                });
            }
        }
        if map
            .insert(id.to_owned(), (rel_type.to_owned(), target.to_owned()))
            .is_some()
        {
            return Err(DocsightError::MalformedDocument {
                message: format!("duplicate relationship Id: {id}"),
            });
        }
    }
    Ok(map)
}

struct DrawingFacts {
    alt_text: Option<String>,
    embedded_image: bool,
    embed: Option<String>,
    shape_type: Option<String>,
    width: Option<String>,
    height: Option<String>,
}

fn drawing_facts(drawing: Node<'_, '_>) -> DrawingFacts {
    let alt_text = drawing
        .descendants()
        .find(|node| node.tag_name().name() == "docPr")
        .and_then(|doc_pr| {
            doc_pr
                .attribute("descr")
                .or_else(|| doc_pr.attribute("title"))
                .or_else(|| doc_pr.attribute("name"))
                .map(str::to_owned)
        });
    let blip = drawing.descendants().find(|node| {
        node.tag_name().name() == "blip"
            && node
                .attribute((R_NS, "embed"))
                .or_else(|| node.attribute("r:embed"))
                .is_some()
    });
    let embed = blip.and_then(|node| {
        node.attribute((R_NS, "embed"))
            .or_else(|| node.attribute("r:embed"))
            .map(str::to_owned)
    });
    let shape_type = drawing.descendants().find_map(|node| {
        if node.tag_name().name() == "prstGeom" {
            node.attribute("prst").map(str::to_owned)
        } else {
            None
        }
    });
    let extent = drawing
        .descendants()
        .find(|node| node.tag_name().name() == "extent");
    DrawingFacts {
        alt_text,
        embedded_image: embed.is_some(),
        embed,
        shape_type,
        width: extent
            .as_ref()
            .and_then(|node| node.attribute("cx"))
            .map(str::to_owned),
        height: extent
            .as_ref()
            .and_then(|node| node.attribute("cy"))
            .map(str::to_owned),
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_figures(
    node: Node<'_, '_>,
    source: &DocumentSource,
    rels: &BTreeMap<String, (String, String)>,
    reading_order: &mut u32,
    figure_index: &mut u32,
    resources: &mut Vec<Resource>,
    binary_part_digests: &BTreeMap<String, String>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Vec<Block>, DocsightError> {
    let mut figures = Vec::new();
    for drawing in node
        .descendants()
        .filter(|n| n.has_tag_name((W_NS, "drawing")) || n.has_tag_name((W_NS, "pict")))
    {
        *figure_index = figure_index.checked_add(1).ok_or_else(block_count_error)?;
        *reading_order = reading_order.checked_add(1).ok_or_else(block_count_error)?;
        let facts = drawing_facts(drawing);
        let mut width_pt = None;
        let mut height_pt = None;
        let alt_text = facts.alt_text;
        let mut resource_id = None;

        if !facts.embedded_image && facts.shape_type.as_deref() == Some("line") {
            let source_path = format!("/word/document.xml::shape[{figure_index}]");
            let shape_id = source.object_id("shape", &source_path);
            warnings.push(Diagnostic {
                code: "DOCX_SHAPE_VISUAL_OMITTED".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!("DrawingML line {shape_id} is preserved without visual projection"),
                effect: "the line's floating position, extent, stroke, and pixels are not reproduced; its structural identity remains available as a shape".to_owned(),
                object: Some(shape_id.clone()),
                page: None,
                occurrences: None,
            });
            figures.push(Block {
                id: shape_id,
                kind: BlockKind::Shape,
                page: None,
                bbox: None,
                z_index: 0,
                reading_order: *reading_order,
                source: SourceSpan::new(&source_path),
                confidence: 1.0,
                flags: LayoutFlags::default(),
                format: Default::default(),
                continuations: Vec::new(),
                inline_runs: Vec::new(),
                content: BlockContent::Shape(ShapeBlock {
                    shape_type: "line".to_owned(),
                    label: alt_text,
                }),
            });
            continue;
        }
        if let Some(cx) = facts.width.as_deref() {
            width_pt = Some(emu_to_points(cx, "figure width")?);
        }
        if let Some(cy) = facts.height.as_deref() {
            height_pt = Some(emu_to_points(cy, "figure height")?);
        }
        if let Some(embed) = facts.embed.as_deref() {
            let unresolved = |reason: String| {
                Diagnostic {
                code: "DOCX_IMAGE_UNRESOLVED".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!("image relationship {embed} is unresolved: {reason}"),
                effect: "text and structure remain exact; the figure is a placeholder without image pixels"
                    .to_owned(),
                object: None,
                page: None,
                occurrences: None,
            }
            };
            let Some((relationship_type, target)) = rels.get(embed) else {
                warnings.push(unresolved(format!(
                    "drawing references missing relationship {embed}"
                )));
                let source_path = format!("/word/document.xml::figure[{figure_index}]");
                let figure_id = source.object_id("fig", &source_path);
                if let Some(last) = warnings.last_mut() {
                    last.object = Some(figure_id.clone());
                }
                figures.push(Block {
                    id: figure_id,
                    kind: BlockKind::Figure,
                    page: None,
                    bbox: None,
                    z_index: 0,
                    reading_order: *reading_order,
                    source: SourceSpan::new(&source_path),
                    confidence: 1.0,
                    flags: LayoutFlags::default(),
                    format: Default::default(),
                    continuations: Vec::new(),
                    inline_runs: Vec::new(),
                    content: BlockContent::Figure(FigureBlock {
                        alt_text: alt_text.clone(),
                        caption: None,
                        resource_id: Some(embed.to_owned()),
                        width_pt,
                        height_pt,
                    }),
                });
                continue;
            };
            if !relationship_type.ends_with("/image") {
                warnings.push(unresolved(format!(
                    "drawing relationship {embed} is not an image"
                )));
                let source_path = format!("/word/document.xml::figure[{figure_index}]");
                let figure_id = source.object_id("fig", &source_path);
                if let Some(last) = warnings.last_mut() {
                    last.object = Some(figure_id.clone());
                }
                figures.push(Block {
                    id: figure_id,
                    kind: BlockKind::Figure,
                    page: None,
                    bbox: None,
                    z_index: 0,
                    reading_order: *reading_order,
                    source: SourceSpan::new(&source_path),
                    confidence: 1.0,
                    flags: LayoutFlags::default(),
                    format: Default::default(),
                    continuations: Vec::new(),
                    inline_runs: Vec::new(),
                    content: BlockContent::Figure(FigureBlock {
                        alt_text: alt_text.clone(),
                        caption: None,
                        resource_id: Some(embed.to_owned()),
                        width_pt,
                        height_pt,
                    }),
                });
                continue;
            }
            let Some(normalized_target) = normalize_internal_target(target) else {
                warnings.push(unresolved(format!(
                    "invalid internal image relationship target: {target}"
                )));
                let source_path = format!("/word/document.xml::figure[{figure_index}]");
                let figure_id = source.object_id("fig", &source_path);
                if let Some(last) = warnings.last_mut() {
                    last.object = Some(figure_id.clone());
                }
                figures.push(Block {
                    id: figure_id,
                    kind: BlockKind::Figure,
                    page: None,
                    bbox: None,
                    z_index: 0,
                    reading_order: *reading_order,
                    source: SourceSpan::new(&source_path),
                    confidence: 1.0,
                    flags: LayoutFlags::default(),
                    format: Default::default(),
                    continuations: Vec::new(),
                    inline_runs: Vec::new(),
                    content: BlockContent::Figure(FigureBlock {
                        alt_text: alt_text.clone(),
                        caption: None,
                        resource_id: Some(embed.to_owned()),
                        width_pt,
                        height_pt,
                    }),
                });
                continue;
            };
            let resource_target = normalized_target.as_str();
            let Some(content_sha256) = binary_part_digests.get(resource_target).cloned() else {
                warnings.push(unresolved(format!(
                    "image relationship {embed} targets missing part {resource_target}"
                )));
                let source_path = format!("/word/document.xml::figure[{figure_index}]");
                let figure_id = source.object_id("fig", &source_path);
                if let Some(last) = warnings.last_mut() {
                    last.object = Some(figure_id.clone());
                }
                figures.push(Block {
                    id: figure_id,
                    kind: BlockKind::Figure,
                    page: None,
                    bbox: None,
                    z_index: 0,
                    reading_order: *reading_order,
                    source: SourceSpan::new(&source_path),
                    confidence: 1.0,
                    flags: LayoutFlags::default(),
                    format: Default::default(),
                    continuations: Vec::new(),
                    inline_runs: Vec::new(),
                    content: BlockContent::Figure(FigureBlock {
                        alt_text: alt_text.clone(),
                        caption: None,
                        resource_id: Some(embed.to_owned()),
                        width_pt,
                        height_pt,
                    }),
                });
                continue;
            };
            let res_id = source.object_id("res", resource_target);
            if !resources.iter().any(|r| r.id == res_id) {
                resources.push(Resource {
                    id: res_id,
                    kind: ResourceKind::Image,
                    name: embed.to_owned(),
                    target: resource_target.to_owned(),
                    mime_type: guess_mime_type(target),
                    content_sha256: Some(content_sha256),
                });
            }
            resource_id = Some(embed.to_owned());
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
            format: Default::default(),
            continuations: Vec::new(),
            inline_runs: Vec::new(),
            content: BlockContent::Figure(FigureBlock {
                alt_text,
                caption: None,
                resource_id,
                width_pt,
                height_pt,
            }),
        });
    }
    Ok(figures)
}

fn emu_to_points(value: &str, field: &str) -> Result<f32, DocsightError> {
    let emu = value
        .parse::<u32>()
        .map_err(|_| DocsightError::MalformedDocument {
            message: format!("{field} is not a supported positive EMU value: {value}"),
        })?;
    if emu == 0 {
        return Err(DocsightError::MalformedDocument {
            message: format!("{field} must be positive"),
        });
    }
    Ok(emu as f32 / 12_700.0)
}

fn figure_warnings<'a>(
    figures: impl Iterator<Item = &'a Block>,
    resources: &[Resource],
    formats: &BTreeMap<String, String>,
    warnings: &mut Vec<Diagnostic>,
) {
    for figure in figures {
        let BlockContent::Figure(content) = &figure.content else {
            continue;
        };
        let target = content.resource_id.as_deref().and_then(|id| {
            resources
                .iter()
                .find(|resource| resource.name == id)
                .map(|resource| resource.target.as_str())
        });
        let format = target
            .and_then(|target| formats.get(target))
            .map(String::as_str)
            .unwrap_or("unknown");
        if matches!(format, "png" | "jpeg") {
            continue;
        }
        warnings.push(Diagnostic {
            code: "DOCX_IMAGE_RASTER_UNAVAILABLE".to_owned(),
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "embedded {format} image for figure {} cannot be rasterized by the selected image decoder",
                figure.id
            ),
            effect: "rendering this figure fails explicitly; no replacement pixels are produced"
                .to_owned(),
            object: Some(figure.id.clone()),
            page: None,
            occurrences: None,
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
                    bbox: None,
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
    let doc = parse_xml(xml)?;
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
            format: Default::default(),
            continuations: Vec::new(),
            inline_runs: Vec::new(),
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
    let doc = parse_xml(xml)?;
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
        occurrences: None,
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
        format: Default::default(),
        continuations: Vec::new(),
        inline_runs: Vec::new(),
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
    "r",
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
    "lastRenderedPageBreak",
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

struct ParagraphSegments {
    texts: Vec<String>,
    leading_page_break: bool,
    trailing_page_break: bool,
}

struct ParagraphFacts {
    segments: ParagraphSegments,
    unsupported: Vec<String>,
}

fn paragraph_facts(node: Node<'_, '_>) -> ParagraphFacts {
    let mut texts = vec![String::new()];
    let mut leading_page_break = false;
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for descendant in node.descendants().filter(Node::is_element) {
        let name = descendant.tag_name().name();
        if let Some(parent) = descendant.parent() {
            if parent == node {
                if !KNOWN_PARAGRAPH_CHILDREN.contains(&name) {
                    unknown.insert(format!("p/{name}"));
                }
            } else if parent.parent() == Some(node)
                && RUN_CONTAINERS.contains(&parent.tag_name().name())
                && !KNOWN_RUN_CHILDREN.contains(&name)
            {
                unknown.insert(format!("{}/{name}", parent.tag_name().name()));
            }
        }
        if name == "oMath" {
            unknown.insert("oMath".to_owned());
        }
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
        if name == "fldSimple" {
            let is_page = descendant.attributes().any(|attribute| {
                (attribute.name() == "instr" || attribute.name().ends_with(":instr"))
                    && attribute.value().to_uppercase().contains("PAGE")
            });
            if is_page && let Some(last) = texts.last_mut() {
                last.push_str("[PAGE]");
            }
            continue;
        }
        if descendant.has_tag_name((W_NS, "t")) {
            if let Some(value) = descendant.text()
                && let Some(last) = texts.last_mut()
            {
                last.push_str(value);
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
    ParagraphFacts {
        segments: ParagraphSegments {
            texts,
            leading_page_break,
            trailing_page_break,
        },
        unsupported: unknown.into_iter().collect(),
    }
}

pub(crate) fn on_off_value(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(value) => !matches!(value, "0" | "false" | "off"),
    }
}

/// Largest distance from a whole number of twentieths of a point that is still read as that
/// whole number. Some generators write values such as `240.00000000000003`.
const TWIPS_ROUNDING_TOLERANCE: f64 = 0.001;
/// Elements and attributes whose values are lengths in twentieths of a point.
const TWIPS_MEASURES: [(&str, &[&str]); 4] = [
    ("spacing", &["before", "after"]),
    (
        "ind",
        &["left", "start", "right", "end", "firstLine", "hanging"],
    ),
    ("pgSz", &["w", "h"]),
    (
        "pgMar",
        &[
            "top", "right", "bottom", "left", "header", "footer", "gutter",
        ],
    ),
];

/// Parses a length in twentieths of a point, reporting whether it had to be rounded.
fn twips_value(value: &str) -> Option<(i64, bool)> {
    let value = value.trim();
    if let Ok(twips) = value.parse::<i64>() {
        return Some((twips, false));
    }
    let (whole, fraction) = value.split_once('.')?;
    let digits = whole.strip_prefix('-').unwrap_or(whole);
    if digits.is_empty()
        || fraction.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let parsed: f64 = value.parse().ok()?;
    let rounded = parsed.round();
    if !rounded.is_finite()
        || (parsed - rounded).abs() > TWIPS_ROUNDING_TOLERANCE
        || rounded.abs() > i64::MAX as f64
    {
        return None;
    }
    Some((rounded as i64, true))
}

/// Counts twips measures written as decimals that are read as a rounded whole value.
fn rounded_twips_measures(root: Node<'_, '_>) -> usize {
    root.descendants()
        .filter(Node::is_element)
        .filter_map(|node| {
            TWIPS_MEASURES
                .iter()
                .find(|(element, _)| node.has_tag_name((W_NS, *element)))
                .map(|(_, attributes)| (node, *attributes))
        })
        .map(|(node, attributes)| {
            attributes
                .iter()
                .filter(|name| {
                    node.attribute((W_NS, **name))
                        .and_then(twips_value)
                        .is_some_and(|(_, rounded)| rounded)
                })
                .count()
        })
        .sum()
}

pub(crate) fn twips_to_points(value: &str, field: &str) -> Result<f32, DocsightError> {
    let (twips, _) = twips_value(value).ok_or_else(|| DocsightError::MalformedDocument {
        message: format!("{field} must be an integer in twentieths of a point"),
    })?;
    let points = twips as f32 / 20.0;
    if !points.is_finite() {
        return Err(DocsightError::MalformedDocument {
            message: format!("{field} is not a finite measurement"),
        });
    }
    Ok(points)
}

fn attribute_points(
    node: Node<'_, '_>,
    names: &[&str],
    field: &str,
) -> Result<Option<f32>, DocsightError> {
    for name in names {
        if let Some(value) = node.attribute((W_NS, *name)) {
            return Ok(Some(twips_to_points(value, field)?));
        }
    }
    Ok(None)
}

fn paragraph_format(properties: Node<'_, '_>) -> Result<ParagraphFormat, DocsightError> {
    let mut format = ParagraphFormat {
        contextual_spacing: child_element(properties, "contextualSpacing")
            .map(|flag| on_off_value(word_value(flag).as_deref())),
        ..Default::default()
    };

    if let Some(justification) = child_element(properties, "jc")
        && let Some(value) = word_value(justification)
    {
        format.alignment = match value.as_str() {
            "left" | "start" => Some(TextAlignment::Left),
            "center" => Some(TextAlignment::Center),
            "right" | "end" => Some(TextAlignment::Right),
            "both" | "distribute" => Some(TextAlignment::Justify),
            _ => None,
        };
    }

    if let Some(spacing) = child_element(properties, "spacing") {
        format.space_before_pt =
            attribute_points(spacing, &["before"], "paragraph spacing before")?;
        format.space_after_pt = attribute_points(spacing, &["after"], "paragraph spacing after")?;
        if let Some(line) = spacing.attribute((W_NS, "line")) {
            let rule = spacing
                .attribute((W_NS, "lineRule"))
                .unwrap_or("auto")
                .to_owned();
            let raw: i64 = line
                .trim()
                .parse()
                .map_err(|_| DocsightError::MalformedDocument {
                    message: "paragraph line spacing must be an integer".to_owned(),
                })?;
            if raw <= 0 {
                return Err(DocsightError::MalformedDocument {
                    message: "paragraph line spacing must be positive".to_owned(),
                });
            }
            match rule.as_str() {
                "auto" => format.line_spacing = Some(raw as f32 / 240.0),
                "exact" | "atLeast" => {
                    format.line_height_pt = Some(raw as f32 / 20.0);
                    format.exact_line_height = rule == "exact";
                }
                _ => {
                    return Err(DocsightError::MalformedDocument {
                        message: format!("unknown paragraph line spacing rule {rule}"),
                    });
                }
            }
        }
    }

    if let Some(indent) = child_element(properties, "ind") {
        format.indent_left_pt = attribute_points(indent, &["left", "start"], "left indent")?;
        format.indent_right_pt = attribute_points(indent, &["right", "end"], "right indent")?;
        let first_line = attribute_points(indent, &["firstLine"], "first line indent")?;
        let hanging = attribute_points(indent, &["hanging"], "hanging indent")?;
        format.indent_first_line_pt = match (first_line, hanging) {
            (_, Some(hanging)) => Some(-hanging),
            (Some(first_line), None) => Some(first_line),
            (None, None) => None,
        };
    }

    Ok(format)
}

fn resolve_paragraph_format(
    node: Node<'_, '_>,
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &ParagraphDefaults,
) -> Result<ParagraphFormat, DocsightError> {
    let mut resolved = defaults.paragraph_format;
    if let Some(id) = style_id.or(defaults.style_id.as_deref()) {
        let mut current = Some(id.to_owned());
        let mut chain = Vec::new();
        let mut depth = 0usize;
        while let Some(key) = current {
            if depth >= MAX_STYLE_DEPTH || chain.contains(&key) {
                break;
            }
            let Some(definition) = styles.get(&key) else {
                break;
            };
            chain.push(key);
            current = definition.based_on.clone();
            depth += 1;
        }
        for key in chain.iter().rev() {
            if let Some(definition) = styles.get(key) {
                merge_paragraph_format(&mut resolved, &definition.paragraph_format);
            }
        }
    }
    if let Some(properties) = child_element(node, "pPr") {
        merge_paragraph_format(&mut resolved, &paragraph_format(properties)?);
    }
    Ok(resolved)
}

fn merge_paragraph_format(target: &mut ParagraphFormat, source: &ParagraphFormat) {
    target.contextual_spacing = source.contextual_spacing.or(target.contextual_spacing);
    if source.line_height_pt.is_some() || source.line_spacing.is_some() {
        target.line_height_pt = source.line_height_pt;
        target.exact_line_height = source.exact_line_height;
        target.line_spacing = source.line_spacing;
    }
    target.alignment = source.alignment.or(target.alignment);
    target.space_before_pt = source.space_before_pt.or(target.space_before_pt);
    target.space_after_pt = source.space_after_pt.or(target.space_after_pt);
    target.line_spacing = source.line_spacing.or(target.line_spacing);
    target.indent_left_pt = source.indent_left_pt.or(target.indent_left_pt);
    target.indent_right_pt = source.indent_right_pt.or(target.indent_right_pt);
    target.indent_first_line_pt = source.indent_first_line_pt.or(target.indent_first_line_pt);
}

fn paragraph_layout_flags(
    node: Node<'_, '_>,
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &ParagraphDefaults,
) -> LayoutFlags {
    let mut resolved = defaults.layout_flags;
    let effective_style = style_id.or(defaults.style_id.as_deref());
    for key in style_chain(effective_style, styles).iter().rev() {
        if let Some(definition) = styles.get(key) {
            resolved.merge(definition.layout_flags);
        }
    }
    if let Some(properties) = child_element(node, "pPr") {
        resolved.merge(StyleLayoutFlags::from_properties(properties));
    }
    resolved.resolve()
}

fn style_chain(style_id: Option<&str>, styles: &BTreeMap<String, StyleDefinition>) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = style_id.map(str::to_owned);
    while let Some(key) = current {
        if chain.len() >= MAX_STYLE_DEPTH || chain.contains(&key) {
            break;
        }
        let Some(definition) = styles.get(&key) else {
            break;
        };
        current = definition.based_on.clone();
        chain.push(key);
    }
    chain
}

#[allow(clippy::too_many_arguments)]
fn parse_paragraph(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &ParagraphDefaults,
    numbering: &NumberingDefinitions,
    reading_order: &mut u32,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Vec<Block>, DocsightError> {
    let base_path = format!("/word/document.xml::body/p[{index}]");
    let style_id = paragraph_property(node, "pStyle").and_then(|property| word_value(property));
    let mut block_format = resolve_paragraph_format(node, style_id.as_deref(), styles, defaults)?;
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
    if let Some(list) = &list
        && let Some(level) = numbering
            .levels
            .get(&(list.numbering_id.clone(), list.level))
    {
        let mut format = level.paragraph_format;
        merge_paragraph_format(&mut format, &block_format);
        block_format = format;
    }
    let facts = paragraph_facts(node);
    let run_segments = paragraph_runs(
        node,
        style_id.as_deref().or(defaults.style_id.as_deref()),
        styles,
        &defaults.run_format,
        &base_path,
    )?;
    let segments = facts.segments;
    let paragraph_flags = paragraph_layout_flags(node, style_id.as_deref(), styles, defaults);
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
                    numbering_id: Some(list.numbering_id.clone()),
                    marker_format: list.format_properties.clone(),
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
            warn_unsupported_run_content(&facts.unsupported, &id, warnings);
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
            format: block_format,
            continuations: Vec::new(),
            inline_runs: run_segments.get(segment_index).cloned().unwrap_or_default(),
            content,
        });
    }
    Ok(blocks)
}

fn warn_unsupported_run_content(
    unknown: &[String],
    block_id: &ObjectId,
    warnings: &mut Vec<Diagnostic>,
) {
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
            occurrences: None,
        });
    }
}

struct TableContext<'a> {
    styles: &'a BTreeMap<String, StyleDefinition>,
    defaults: &'a ParagraphDefaults,
    numbering: &'a NumberingDefinitions,
    header_tokens: Option<&'a BTreeMap<usize, Vec<(usize, String)>>>,
}

fn parse_table(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    reading_order: u32,
    context: &TableContext<'_>,
) -> Result<Block, DocsightError> {
    let source_path = format!("/word/document.xml::body/tbl[{index}]");
    let (id, table_block, span) = parse_table_at(node, source_path, source, 0, context)?;
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
        format: Default::default(),
        continuations: Vec::new(),
        inline_runs: Vec::new(),
        content: BlockContent::Table(table_block),
    })
}

fn parse_table_at(
    node: Node<'_, '_>,
    source_path: String,
    source: &DocumentSource,
    depth: usize,
    context: &TableContext<'_>,
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
    let mut cell_patches = Vec::new();
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
        let mut row_is_header = child_element(row_node, "trPr")
            .and_then(|node| child_element(node, "tblHeader"))
            .is_some_and(|node| on_off_value(word_value(node).as_deref()));
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

            let mut nested_tables = Vec::new();
            let mut paragraph_index = 0;
            let mut table_index = 0;
            let mut order = 0;
            for child in cell_node.children().filter(Node::is_element) {
                if child.has_tag_name((W_NS, "tbl")) {
                    table_index += 1;
                    let (id, table, span) = parse_table_at(
                        child,
                        format!("{cell_source}/tbl[{table_index}]"),
                        source,
                        depth + 1,
                        context,
                    )?;
                    order += 1;
                    nested_tables.push(Block {
                        id,
                        kind: BlockKind::Table,
                        page: None,
                        bbox: None,
                        z_index: 0,
                        reading_order: order,
                        source: span,
                        confidence: 1.0,
                        flags: LayoutFlags::default(),
                        format: ParagraphFormat::default(),
                        continuations: Vec::new(),
                        inline_runs: Vec::new(),
                        content: BlockContent::Table(table),
                    });
                } else if child.has_tag_name((W_NS, "p")) {
                    paragraph_index += 1;
                    let path = format!("{cell_source}/p[{paragraph_index}]");
                    let mut parsed = if let Some(tokens) = context.header_tokens {
                        order += 1;
                        vec![formatted_header_paragraph(
                            child,
                            tokens.get(&child.range().start).ok_or_else(|| {
                                DocsightError::MalformedDocument {
                                    message: "header table paragraph has no decoded tokens"
                                        .to_owned(),
                                }
                            })?,
                            path.clone(),
                            order,
                            source,
                            context.styles,
                            context.defaults,
                        )?]
                    } else {
                        parse_paragraph(
                            child,
                            paragraph_index,
                            source,
                            context.styles,
                            context.defaults,
                            context.numbering,
                            &mut order,
                            &mut Vec::new(),
                        )?
                    };
                    for (segment, block) in parsed.iter_mut().enumerate() {
                        let path = if segment == 0 {
                            path.clone()
                        } else {
                            format!("{path}::part[{}]", segment + 1)
                        };
                        block.source = SourceSpan::new(path.clone());
                        block.id = source.object_id("p", &path);
                        for run in &mut block.inline_runs {
                            run.source = block.source.clone();
                        }
                    }
                    nested_tables.extend(parsed);
                }
            }
            let text = nested_tables
                .iter()
                .map(Block::text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
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
                    cell_patches.push(
                        child_element(cell_node, "tcPr")
                            .map(cell_format_patch)
                            .transpose()?
                            .unwrap_or_default(),
                    );
                    cells.push(TableCell {
                        id: source.object_id("cell", &cell_source),
                        row,
                        column,
                        row_span: 1,
                        column_span,
                        bbox: None,
                        page: None,
                        continuations: Vec::new(),
                        format: Default::default(),
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
    let column_widths_pt = table_grid_widths(node)?;
    let properties = child_element(node, "tblPr");
    let style_id = properties
        .and_then(|node| child_element(node, "tblStyle"))
        .and_then(word_value);
    let table_patch = properties
        .map(cell_format_patch)
        .transpose()?
        .unwrap_or_default();
    for (cell, patch) in cells.iter_mut().zip(cell_patches) {
        for style in style_chain(style_id.as_deref(), context.styles)
            .iter()
            .rev()
        {
            if let Some(style) = context.styles.get(style) {
                apply_cell_patch(
                    &mut cell.format,
                    &style.cell_format,
                    cell.row,
                    cell.column,
                    rows,
                    columns,
                );
            }
        }
        apply_cell_patch(
            &mut cell.format,
            &table_patch,
            cell.row,
            cell.column,
            rows,
            columns,
        );
        apply_cell_patch(
            &mut cell.format,
            &patch,
            cell.row,
            cell.column,
            rows,
            columns,
        );
    }
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
            detector: None,
        },
        span,
    ))
}

fn table_grid_widths(node: Node<'_, '_>) -> Result<Option<Vec<f32>>, DocsightError> {
    let Some(grid) = child_element(node, "tblGrid") else {
        return Ok(None);
    };
    let widths =
        grid.children()
            .filter(|child| child.has_tag_name((W_NS, "gridCol")))
            .map(|column| {
                let value = column.attribute((W_NS, "w")).ok_or_else(|| {
                    DocsightError::MalformedDocument {
                        message: "table grid column has no width".to_owned(),
                    }
                })?;
                let width = twips_to_points(value, "table grid column width")?;
                if width <= 0.0 {
                    return Err(DocsightError::MalformedDocument {
                        message: "table grid column width must be positive".to_owned(),
                    });
                }
                Ok(width)
            })
            .collect::<Result<Vec<_>, DocsightError>>()?;
    if widths.is_empty() {
        return Err(DocsightError::MalformedDocument {
            message: "explicit table grid has no columns".to_owned(),
        });
    }
    Ok(Some(widths))
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

fn parse_styles(
    document: Option<&XmlDocument<'_>>,
) -> Result<BTreeMap<String, StyleDefinition>, DocsightError> {
    let Some(document) = document else {
        return Ok(BTreeMap::new());
    };
    let mut styles = BTreeMap::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "style")))
    {
        if !matches!(
            node.attribute((W_NS, "type")),
            Some("paragraph" | "character" | "table")
        ) {
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
        let run_properties = child_element(node, "rPr");
        let font_family = run_properties
            .and_then(|properties| child_element(properties, "rFonts"))
            .and_then(|fonts| {
                fonts
                    .attribute((W_NS, "ascii"))
                    .or_else(|| fonts.attribute((W_NS, "hAnsi")))
                    .map(str::to_owned)
            });
        let font_size_pt = run_properties
            .and_then(|properties| child_element(properties, "sz"))
            .map(parse_half_point_size)
            .transpose()?;
        let bold = run_properties
            .and_then(|properties| child_element(properties, "b"))
            .map(|value| on_off_value(word_value(value).as_deref()));
        let italic = run_properties
            .and_then(|properties| child_element(properties, "i"))
            .map(|value| on_off_value(word_value(value).as_deref()));
        styles.insert(
            style_id.to_owned(),
            StyleDefinition {
                name,
                based_on,
                paragraph_format: match child_element(node, "pPr") {
                    Some(properties) => paragraph_format(properties)?,
                    None => ParagraphFormat::default(),
                },
                outline_level,
                numbering,
                font_family,
                font_size_pt,
                bold,
                italic,
                color_argb: run_properties
                    .map(run_format)
                    .transpose()?
                    .and_then(|format| format.color_argb),
                underline: run_properties
                    .map(run_format)
                    .transpose()?
                    .and_then(|format| format.underline),
                layout_flags: child_element(node, "pPr")
                    .map(StyleLayoutFlags::from_properties)
                    .unwrap_or_default(),
                cell_format: child_element(node, "tcPr")
                    .or_else(|| child_element(node, "tblPr"))
                    .map(cell_format_patch)
                    .transpose()?
                    .unwrap_or_default(),
            },
        );
    }
    Ok(styles)
}

fn parse_paragraph_defaults(
    document: Option<&XmlDocument<'_>>,
) -> Result<ParagraphDefaults, DocsightError> {
    let Some(document) = document else {
        return Ok(ParagraphDefaults::default());
    };
    let root = document.root_element();
    let layout_flags = child_element(root, "docDefaults")
        .and_then(|defaults| child_element(defaults, "pPrDefault"))
        .and_then(|defaults| child_element(defaults, "pPr"))
        .map(StyleLayoutFlags::from_properties)
        .unwrap_or_default();
    let style_id = root
        .children()
        .filter(|node| node.has_tag_name((W_NS, "style")))
        .find(|node| {
            node.attribute((W_NS, "type")) == Some("paragraph")
                && node
                    .attribute((W_NS, "default"))
                    .is_some_and(|value| on_off_value(Some(value)))
        })
        .and_then(|node| node.attribute((W_NS, "styleId")))
        .map(str::to_owned);
    Ok(ParagraphDefaults {
        style_id,
        layout_flags,
        run_format: child_element(root, "docDefaults")
            .and_then(|defaults| child_element(defaults, "rPrDefault"))
            .and_then(|defaults| child_element(defaults, "rPr"))
            .map(run_format)
            .transpose()?
            .unwrap_or_default(),
        paragraph_format: child_element(root, "docDefaults")
            .and_then(|node| child_element(node, "pPrDefault"))
            .and_then(|node| child_element(node, "pPr"))
            .map(paragraph_format)
            .transpose()?
            .unwrap_or_default(),
    })
}

fn merge_run_format(target: &mut RunFormat, source: &RunFormat) {
    target.font_family = source.font_family.clone().or(target.font_family.clone());
    target.font_size_pt = source.font_size_pt.or(target.font_size_pt);
    target.bold = source.bold.or(target.bold);
    target.italic = source.italic.or(target.italic);
    target.color_argb = source.color_argb.or(target.color_argb);
    target.underline = source.underline.or(target.underline);
}

fn formatted_header_paragraph(
    paragraph: Node<'_, '_>,
    tokens: &[(usize, String)],
    path: String,
    order: u32,
    source: &DocumentSource,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &ParagraphDefaults,
) -> Result<Block, DocsightError> {
    let style_id = paragraph_property(paragraph, "pStyle")
        .and_then(word_value)
        .or(defaults.style_id.clone());
    let mut runs = Vec::new();
    for (position, text) in tokens.iter().cloned() {
        let run = paragraph
            .descendants()
            .find(|node| node.range().start == position && node.is_element())
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: format!("header text has no source run in {path}"),
            })?;
        let mut format = defaults.run_format.clone();
        if let Some(style) = &style_id {
            let style = resolve_style_formatting(style, styles)?;
            merge_run_format(
                &mut format,
                &RunFormat {
                    font_family: style.font_family,
                    font_size_pt: style.font_size_pt,
                    bold: style.bold,
                    italic: style.italic,
                    color_argb: style.color_argb,
                    underline: style.underline,
                },
            );
        }
        if let Some(properties) = child_element(run, "rPr") {
            merge_run_format(&mut format, &run_format(properties)?);
        }
        runs.push(InlineRun {
            text,
            format,
            source: SourceSpan::new(format!("{path}/r@{position}")),
        });
    }
    let text = runs.iter().map(|run| run.text.as_str()).collect::<String>();
    Ok(Block {
        id: source.object_id("hdrp", &path),
        kind: BlockKind::Paragraph,
        page: None,
        bbox: None,
        z_index: 0,
        reading_order: order,
        source: SourceSpan::new(path),
        confidence: 1.0,
        flags: paragraph_layout_flags(paragraph, style_id.as_deref(), styles, defaults),
        format: resolve_paragraph_format(paragraph, style_id.as_deref(), styles, defaults)?,
        continuations: Vec::new(),
        inline_runs: runs,
        content: BlockContent::Paragraph(ParagraphBlock { text, style_id }),
    })
}

fn formatted_header_footer(
    xml: &str,
    part: &str,
    source: &DocumentSource,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &ParagraphDefaults,
) -> Result<Vec<Block>, DocsightError> {
    let parsed = parse_xml(xml)?;
    let decoded = crate::sections::header_footer_part(xml)?;
    let tokens = parsed
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "p")))
        .zip(decoded.paragraphs)
        .map(|(node, tokens)| (node.range().start, tokens))
        .collect::<BTreeMap<_, _>>();
    let numbering = NumberingDefinitions::default();
    let context = TableContext {
        styles,
        defaults,
        numbering: &numbering,
        header_tokens: Some(&tokens),
    };
    let mut blocks = Vec::new();
    let mut paragraph_index = 0;
    let mut table_index = 0;
    for node in parsed.root_element().children().filter(Node::is_element) {
        let order = u32::try_from(blocks.len() + 1).map_err(|_| block_count_error())?;
        if node.has_tag_name((W_NS, "p")) {
            paragraph_index += 1;
            let content = tokens.get(&node.range().start).ok_or_else(|| {
                DocsightError::MalformedDocument {
                    message: format!("header paragraph has no decoded content in {part}"),
                }
            })?;
            blocks.push(formatted_header_paragraph(
                node,
                content,
                format!("{part}::p[{paragraph_index}]"),
                order,
                source,
                styles,
                defaults,
            )?);
        } else if node.has_tag_name((W_NS, "tbl")) {
            table_index += 1;
            let (id, table, span) = parse_table_at(
                node,
                format!("{part}::tbl[{table_index}]"),
                source,
                0,
                &context,
            )?;
            blocks.push(Block {
                id,
                kind: BlockKind::Table,
                page: None,
                bbox: None,
                z_index: 0,
                reading_order: order,
                source: span,
                confidence: 1.0,
                flags: LayoutFlags::default(),
                format: ParagraphFormat::default(),
                continuations: Vec::new(),
                inline_runs: Vec::new(),
                content: BlockContent::Table(table),
            });
        } else {
            return Err(DocsightError::UnsupportedFeature {
                feature: format!("header/footer element {} in {part}", node.tag_name().name()),
            });
        }
    }
    Ok(blocks)
}

fn run_format(properties: Node<'_, '_>) -> Result<RunFormat, DocsightError> {
    let font_family = child_element(properties, "rFonts")
        .and_then(|fonts| {
            fonts
                .attribute((W_NS, "ascii"))
                .or_else(|| fonts.attribute((W_NS, "hAnsi")))
        })
        .map(str::to_owned);
    let color_argb = child_element(properties, "color")
        .and_then(word_value)
        .filter(|value| value != "auto")
        .map(|value| {
            u32::from_str_radix(&value, 16)
                .map(|color| color | 0xff000000)
                .map_err(|_| DocsightError::MalformedDocument {
                    message: format!("invalid run color {value}"),
                })
        })
        .transpose()?;
    Ok(RunFormat {
        font_family,
        font_size_pt: child_element(properties, "sz")
            .map(parse_half_point_size)
            .transpose()?,
        bold: child_element(properties, "b").map(|node| on_off_value(word_value(node).as_deref())),
        italic: child_element(properties, "i")
            .map(|node| on_off_value(word_value(node).as_deref())),
        color_argb,
        underline: child_element(properties, "u")
            .map(|node| word_value(node).as_deref() != Some("none")),
    })
}

fn paragraph_runs(
    node: Node<'_, '_>,
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
    defaults: &RunFormat,
    path: &str,
) -> Result<Vec<Vec<InlineRun>>, DocsightError> {
    let mut base = defaults.clone();
    if let Some(style_id) = style_id {
        let resolved = resolve_style_formatting(style_id, styles)?;
        merge_run_format(
            &mut base,
            &RunFormat {
                font_family: resolved.font_family,
                font_size_pt: resolved.font_size_pt,
                bold: resolved.bold,
                italic: resolved.italic,
                color_argb: resolved.color_argb,
                underline: resolved.underline,
            },
        );
    }
    let mut segments = vec![Vec::<InlineRun>::new()];
    for (index, run) in node
        .descendants()
        .filter(|run| run.has_tag_name((W_NS, "r")))
        .enumerate()
    {
        if run
            .ancestors()
            .any(|ancestor| ancestor.has_tag_name((W_NS, "del")))
        {
            continue;
        }
        let mut format = base.clone();
        if let Some(properties) = child_element(run, "rPr") {
            if let Some(style) = child_element(properties, "rStyle").and_then(word_value) {
                let resolved = resolve_style_formatting(&style, styles)?;
                merge_run_format(
                    &mut format,
                    &RunFormat {
                        font_family: resolved.font_family,
                        font_size_pt: resolved.font_size_pt,
                        bold: resolved.bold,
                        italic: resolved.italic,
                        color_argb: resolved.color_argb,
                        underline: resolved.underline,
                    },
                );
            }
            merge_run_format(&mut format, &run_format(properties)?);
        }
        let mut text = String::new();
        for child in run.children().filter(Node::is_element) {
            match child.tag_name().name() {
                "t" => text.push_str(child.text().unwrap_or_default()),
                "tab" => text.push('\t'),
                "br" if child.attribute((W_NS, "type")) == Some("page") => {
                    if !text.is_empty()
                        && let Some(segment) = segments.last_mut()
                    {
                        segment.push(InlineRun {
                            text: std::mem::take(&mut text),
                            format: format.clone(),
                            source: SourceSpan::new(format!("{path}/r[{}]", index + 1)),
                        });
                    }
                    if segments.last().is_some_and(|segment| !segment.is_empty()) {
                        segments.push(Vec::new());
                    }
                }
                "br" | "cr" => text.push('\n'),
                _ => {}
            }
        }
        if !text.is_empty()
            && let Some(segment) = segments.last_mut()
        {
            segment.push(InlineRun {
                text,
                format,
                source: SourceSpan::new(format!("{path}/r[{}]", index + 1)),
            });
        }
    }
    Ok(segments)
}

fn parse_half_point_size(node: Node<'_, '_>) -> Result<f32, DocsightError> {
    let value = word_value(node).ok_or_else(|| DocsightError::MalformedDocument {
        message: "style font size has no value".to_owned(),
    })?;
    let half_points = value
        .parse::<u32>()
        .map_err(|_| DocsightError::MalformedDocument {
            message: format!("style font size is not an unsigned integer: {value}"),
        })?;
    if !(1..=3276).contains(&half_points) {
        return Err(DocsightError::MalformedDocument {
            message: format!("style font size is invalid: {value}"),
        });
    }
    Ok(half_points as f32 / 2.0)
}

fn resolve_style_formatting(
    style_id: &str,
    styles: &BTreeMap<String, StyleDefinition>,
) -> Result<ResolvedStyleFormatting, DocsightError> {
    let mut chain = Vec::new();
    let mut current = Some(style_id);
    let mut visited = BTreeSet::new();
    while let Some(id) = current {
        if !visited.insert(id.to_owned()) {
            return Err(DocsightError::MalformedDocument {
                message: format!("cycle detected in style inheritance: {id}"),
            });
        }
        if chain.len() >= MAX_STYLE_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "style inheritance depth".to_owned(),
                limit: MAX_STYLE_DEPTH as u64,
            });
        }
        let Some(definition) = styles.get(id) else {
            break;
        };
        chain.push(definition);
        current = definition.based_on.as_deref();
    }
    let mut resolved = ResolvedStyleFormatting::default();
    for definition in chain.into_iter().rev() {
        if let Some(value) = &definition.font_family {
            resolved.font_family = Some(value.clone());
        }
        if let Some(value) = definition.font_size_pt {
            resolved.font_size_pt = Some(value);
        }
        if let Some(value) = definition.bold {
            resolved.bold = Some(value);
        }
        if let Some(value) = definition.italic {
            resolved.italic = Some(value);
        }
        resolved.color_argb = definition.color_argb.or(resolved.color_argb);
        resolved.underline = definition.underline.or(resolved.underline);
    }
    Ok(resolved)
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
    let document = parse_xml(xml)?;
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
            levels.insert(
                level,
                NumberingLevel {
                    format,
                    pattern,
                    start: child_element(level_node, "start")
                        .and_then(word_value)
                        .map(|value| {
                            value
                                .parse::<u32>()
                                .map_err(|_| DocsightError::MalformedDocument {
                                    message: "numbering start must be an unsigned integer"
                                        .to_owned(),
                                })
                        })
                        .transpose()?
                        .unwrap_or(1),
                    format_properties: child_element(level_node, "rPr")
                        .map(run_format)
                        .transpose()?
                        .unwrap_or_default(),
                    paragraph_format: child_element(level_node, "pPr")
                        .map(paragraph_format)
                        .transpose()?
                        .unwrap_or_default(),
                },
            );
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
                levels.insert((num_id.to_owned(), *level), definition.clone());
            }
        }
        for override_node in node
            .children()
            .filter(|node| node.has_tag_name((W_NS, "lvlOverride")))
        {
            let level = override_node
                .attribute((W_NS, "ilvl"))
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "numbering override has no level".to_owned(),
                })?
                .parse::<u8>()
                .map_err(|_| DocsightError::MalformedDocument {
                    message: "numbering override level must be an integer".to_owned(),
                })?;
            if let Some(start) = child_element(override_node, "startOverride").and_then(word_value)
            {
                let start = start
                    .parse::<u32>()
                    .map_err(|_| DocsightError::MalformedDocument {
                        message: "numbering start override must be an unsigned integer".to_owned(),
                    })?;
                let definition = levels.get_mut(&(num_id.to_owned(), level)).ok_or_else(|| {
                    DocsightError::MalformedDocument {
                        message: "numbering start override has no level definition".to_owned(),
                    }
                })?;
                definition.start = start;
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
        .and_then(|_| numbering.levels.get(&(num_id.clone(), level)));
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
        numbering_id: num_id,
        format_properties: definition.map(|definition| definition.format_properties.clone()),
        level,
        format,
        pattern,
        ordered,
    })
}

fn realize_numbering(
    blocks: &mut [Block],
    definitions: &NumberingDefinitions,
    counters: &mut BTreeMap<(String, u8), u32>,
) -> Result<(), DocsightError> {
    for block in blocks {
        match &mut block.content {
            BlockContent::ListItem(item) => {
                let Some(id) = item.numbering_id.as_deref() else {
                    continue;
                };
                let key = (id.to_owned(), item.level);
                let Some(definition) = definitions.levels.get(&key) else {
                    continue;
                };
                if block.source.path.contains("::part[") {
                    item.marker = None;
                    continue;
                }
                let next = match counters.get(&key) {
                    Some(previous) => {
                        previous
                            .checked_add(1)
                            .ok_or_else(|| DocsightError::ResourceLimit {
                                resource: "list numbering counter".to_owned(),
                                limit: u64::from(u32::MAX),
                            })?
                    }
                    None => definition.start,
                };
                counters.retain(|(list, level), _| list != id || *level <= item.level);
                counters.insert(key, next);
                if item.format.as_deref() == Some("none") {
                    item.marker = None;
                    continue;
                }
                if let Some(pattern) = &item.pattern {
                    let mut marker = pattern.clone();
                    for level in 0..9_u8 {
                        let placeholder = format!("%{}", level + 1);
                        if !marker.contains(&placeholder) {
                            continue;
                        }
                        let level_definition = definitions
                            .levels
                            .get(&(id.to_owned(), level))
                            .ok_or_else(|| DocsightError::MalformedDocument {
                                message: format!("list marker references undefined level {level}"),
                            })?;
                        let number = counters
                            .get(&(id.to_owned(), level))
                            .copied()
                            .unwrap_or(level_definition.start);
                        let text = match level_definition.format.as_deref() {
                            Some("decimal") => number.to_string(),
                            Some("decimalZero") => format!("{number:02}"),
                            Some("lowerRoman" | "upperRoman" | "lowerLetter" | "upperLetter") => {
                                docsight_core::format_ooxml_number(
                                    number,
                                    level_definition.format.as_deref(),
                                )?
                            }
                            Some("bullet") => continue,
                            other => {
                                return Err(DocsightError::UnsupportedFeature {
                                    feature: format!("DOCX numbering format {other:?}"),
                                });
                            }
                        };
                        marker = marker.replace(&placeholder, &text);
                    }
                    item.marker = Some(marker);
                }
            }
            BlockContent::Table(table) => {
                for cell in &mut table.cells {
                    realize_numbering(&mut cell.blocks, definitions, counters)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
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

pub(crate) fn child_element<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|child| child.has_tag_name((W_NS, name)))
}

pub(crate) fn word_value(node: Node<'_, '_>) -> Option<String> {
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

pub(crate) fn parse_xml(xml: &str) -> Result<XmlDocument<'_>, DocsightError> {
    preflight_xml(xml)?;
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_XML_NODES,
        entity_resolver: None,
    };
    let document = XmlDocument::parse_with_options(xml, options).map_err(xml_error)?;
    for node in document.descendants() {
        let depth = node
            .ancestors()
            .filter(Node::is_element)
            .take(MAX_XML_DEPTH + 1)
            .count();
        if depth > MAX_XML_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "XML element depth".to_owned(),
                limit: MAX_XML_DEPTH as u64,
            });
        }
        if node
            .text()
            .is_some_and(|text| text.len() > MAX_XML_TOKEN_BYTES)
        {
            return Err(xml_token_limit());
        }
        if node.is_element() {
            if node.tag_name().name().len() > MAX_XML_TOKEN_BYTES {
                return Err(xml_token_limit());
            }
            for attribute in node.attributes() {
                if attribute.name().len() > MAX_XML_TOKEN_BYTES
                    || attribute.value().len() > MAX_XML_TOKEN_BYTES
                {
                    return Err(xml_token_limit());
                }
            }
        }
    }
    Ok(document)
}

fn preflight_xml(xml: &str) -> Result<(), DocsightError> {
    let bytes = xml.as_bytes();
    let mut cursor = 0_usize;
    let mut depth = 0_usize;
    let mut nodes = 1_u32;
    while cursor < bytes.len() {
        let Some(relative_start) = bytes[cursor..].iter().position(|byte| *byte == b'<') else {
            validate_xml_token_length(bytes.len().saturating_sub(cursor))?;
            break;
        };
        let start = cursor
            .checked_add(relative_start)
            .ok_or_else(xml_token_limit)?;
        validate_xml_token_length(start.saturating_sub(cursor))?;
        let marker =
            bytes
                .get(start + 1)
                .copied()
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "invalid OOXML: unterminated XML markup".to_owned(),
                })?;
        if marker == b'!' {
            let (end, counted) = if bytes[start..].starts_with(b"<!--") {
                (find_xml_sequence(bytes, start + 4, b"-->")?, true)
            } else if bytes[start..].starts_with(b"<![CDATA[") {
                (find_xml_sequence(bytes, start + 9, b"]]>")?, true)
            } else {
                return Err(DocsightError::MalformedDocument {
                    message: "invalid OOXML: DTD or entity declarations are forbidden".to_owned(),
                });
            };
            validate_xml_token_length(end.saturating_sub(start))?;
            if counted {
                increment_xml_nodes(&mut nodes)?;
            }
            cursor = end;
            continue;
        }
        if marker == b'?' {
            let end = find_xml_sequence(bytes, start + 2, b"?>")?;
            validate_xml_token_length(end.saturating_sub(start))?;
            increment_xml_nodes(&mut nodes)?;
            cursor = end;
            continue;
        }
        let end = find_xml_tag_end(bytes, start + 1)?;
        validate_xml_token_length(end.saturating_sub(start))?;
        if marker == b'/' {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| DocsightError::MalformedDocument {
                    message: "invalid OOXML: closing tag has no open element".to_owned(),
                })?;
        } else {
            increment_xml_nodes(&mut nodes)?;
            if !xml_tag_is_self_closing(&bytes[start + 1..end]) {
                depth = depth.checked_add(1).ok_or_else(xml_depth_limit)?;
                if depth > MAX_XML_DEPTH {
                    return Err(xml_depth_limit());
                }
            }
        }
        cursor = end.checked_add(1).ok_or_else(xml_token_limit)?;
    }
    Ok(())
}

fn find_xml_tag_end(bytes: &[u8], mut cursor: usize) -> Result<usize, DocsightError> {
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        match (quote, byte) {
            (Some(active), current) if active == current => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Ok(cursor),
            _ => {}
        }
        cursor = cursor.checked_add(1).ok_or_else(xml_token_limit)?;
    }
    Err(DocsightError::MalformedDocument {
        message: "invalid OOXML: unterminated XML tag".to_owned(),
    })
}

fn find_xml_sequence(
    bytes: &[u8],
    mut cursor: usize,
    sequence: &[u8],
) -> Result<usize, DocsightError> {
    while cursor <= bytes.len().saturating_sub(sequence.len()) {
        if bytes[cursor..].starts_with(sequence) {
            return cursor
                .checked_add(sequence.len())
                .ok_or_else(xml_token_limit);
        }
        cursor = cursor.checked_add(1).ok_or_else(xml_token_limit)?;
    }
    Err(DocsightError::MalformedDocument {
        message: "invalid OOXML: unterminated XML declaration".to_owned(),
    })
}

fn xml_tag_is_self_closing(tag: &[u8]) -> bool {
    tag.iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'/')
}

fn validate_xml_token_length(length: usize) -> Result<(), DocsightError> {
    if length > MAX_XML_TOKEN_BYTES {
        return Err(xml_token_limit());
    }
    Ok(())
}

fn increment_xml_nodes(nodes: &mut u32) -> Result<(), DocsightError> {
    *nodes = nodes.checked_add(1).ok_or_else(xml_node_limit)?;
    if *nodes > MAX_XML_NODES {
        return Err(xml_node_limit());
    }
    Ok(())
}

fn xml_depth_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "XML element depth".to_owned(),
        limit: MAX_XML_DEPTH as u64,
    }
}

fn xml_node_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "XML node count".to_owned(),
        limit: u64::from(MAX_XML_NODES),
    }
}

fn xml_error(error: roxmltree::Error) -> DocsightError {
    match error {
        roxmltree::Error::NodesLimitReached => xml_node_limit(),
        other => DocsightError::MalformedDocument {
            message: format!("invalid OOXML: {other}"),
        },
    }
}

fn xml_token_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "XML token bytes".to_owned(),
        limit: MAX_XML_TOKEN_BYTES as u64,
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
