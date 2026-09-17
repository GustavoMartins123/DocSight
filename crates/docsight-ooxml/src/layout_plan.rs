use crate::package::read_parts;
use crate::parser::{MAX_XML_DEPTH, MAX_XML_NODES};
use docsight_core::{DocumentFormat, DocumentSource, DocsightError, Section};
use roxmltree::{Document as XmlDocument, Node, ParsingOptions};
use std::collections::BTreeMap;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

#[derive(Clone, Debug, PartialEq)]
pub struct SectionRun {
    pub start_block: usize,
    pub end_block: usize,
    pub section: Section,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocxLayoutPlan {
    pub sections: Vec<SectionRun>,
}

pub fn build_layout_plan(source: &DocumentSource) -> Result<DocxLayoutPlan, DocsightError> {
    if source.format() != DocumentFormat::Docx {
        return Err(DocsightError::UnsupportedOperation {
            operation: "DOCX layout planning".to_owned(),
            format: source.format(),
        });
    }

    let parts = read_parts(source.bytes())?;
    let xml = parse_xml(&parts.document)?;
    let body = xml.descendants().find(|node| node.has_tag_name((W_NS, "body"))).ok_or_else(|| DocsightError::MalformedDocument { message: "word/document.xml has no w:body".to_owned() })?;
    let relationships = relationship_targets(parts.rels.as_deref())?;
    let headers = part_texts(&parts.headers)?;
    let footers = part_texts(&parts.footers)?;
    let mut runs = Vec::new();
    let mut start_block = 0_usize;
    let mut block_cursor = 0_usize;
    let mut section_index = 0_u32;
    let mut paragraph_index = 0_u32;
    let mut body_section_index = 0_u32;
    let mut previous: Option<Section> = None;

    for child in body.children().filter(|node| node.is_element()) {
        if child.has_tag_name((W_NS, "p")) {
            paragraph_index = paragraph_index.checked_add(1).ok_or_else(count_error)?;
            let paragraph_blocks = paragraph_block_count(child);
            block_cursor = block_cursor.checked_add(paragraph_blocks).and_then(|value| value.checked_add(figure_count(child))).ok_or_else(count_error)?;
            if let Some(section_node) = child.children().find(|node| node.has_tag_name((W_NS, "pPr"))).and_then(|properties| properties.children().find(|node| node.has_tag_name((W_NS, "sectPr")))) {
                section_index = section_index.checked_add(1).ok_or_else(count_error)?;
                let path = format!("/word/document.xml::body/p[{paragraph_index}]/pPr/sectPr");
                let section = parse_section(section_node, section_index, &path, source, &relationships, &headers, &footers, previous.as_ref())?;
                runs.push(SectionRun { start_block, end_block: block_cursor, section: section.clone() });
                previous = Some(section);
                start_block = block_cursor;
            }
        } else if child.has_tag_name((W_NS, "tbl")) {
            block_cursor = block_cursor.checked_add(1).ok_or_else(count_error)?;
        } else if child.has_tag_name((W_NS, "sectPr")) {
            body_section_index = body_section_index.checked_add(1).ok_or_else(count_error)?;
            section_index = section_index.checked_add(1).ok_or_else(count_error)?;
            let path = format!("/word/document.xml::body/sectPr[{body_section_index}]");
            let section = parse_section(child, section_index, &path, source, &relationships, &headers, &footers, previous.as_ref())?;
            if block_cursor > start_block || runs.is_empty() {
                runs.push(SectionRun { start_block, end_block: block_cursor, section: section.clone() });
                start_block = block_cursor;
            }
            previous = Some(section);
        } else {
            block_cursor = block_cursor.checked_add(1).ok_or_else(count_error)?;
        }
    }

    if runs.is_empty() {
        runs.push(SectionRun { start_block: 0, end_block: block_cursor, section: default_section(source, 1) });
    } else if start_block < block_cursor {
        let section = previous.unwrap_or_else(|| default_section(source, section_index + 1));
        runs.push(SectionRun { start_block, end_block: block_cursor, section });
    }
    Ok(DocxLayoutPlan { sections: runs })
}

fn parse_xml(xml: &str) -> Result<XmlDocument<'_>, DocsightError> {
    let options = ParsingOptions { allow_dtd: false, nodes_limit: MAX_XML_NODES, entity_resolver: None };
    let document = XmlDocument::parse_with_options(xml, options).map_err(|error| DocsightError::MalformedDocument { message: format!("invalid OOXML XML: {error}") })?;
    let too_deep = document.descendants().any(|node| node.is_element() && node.ancestors().filter(|ancestor| ancestor.is_element()).count() > MAX_XML_DEPTH);
    if too_deep { return Err(DocsightError::ResourceLimit { resource: "XML element depth".to_owned(), limit: MAX_XML_DEPTH as u64 }); }
    Ok(document)
}

fn relationship_targets(xml: Option<&str>) -> Result<BTreeMap<String, String>, DocsightError> {
    let mut targets = BTreeMap::new();
    let Some(xml) = xml else { return Ok(targets); };
    let document = parse_xml(xml)?;
    for node in document.descendants().filter(|node| node.is_element()) {
        if node.tag_name().name() != "Relationship" || node.attribute("TargetMode") == Some("External") { continue; }
        let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target")) else { continue; };
        targets.insert(id.to_owned(), normalize_word_target(target));
    }
    Ok(targets)
}

fn normalize_word_target(target: &str) -> String {
    let normalized = target.replace('\\', "/");
    if normalized.starts_with("word/") { return normalized; }
    let mut components = vec!["word".to_owned()];
    for component in normalized.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => { if components.len() > 1 { components.pop(); } }
            value => components.push(value.to_owned()),
        }
    }
    components.join("/")
}

fn part_texts(parts: &[(String, String)]) -> Result<BTreeMap<String, String>, DocsightError> {
    let mut values = BTreeMap::new();
    for (name, xml) in parts {
        let document = parse_xml(xml)?;
        let text = document.descendants().filter(|node| node.has_tag_name((W_NS, "t"))).filter_map(|node| node.text()).map(str::trim).filter(|value| !value.is_empty()).collect::<Vec<_>>().join(" ");
        if !text.is_empty() { values.insert(name.replace('\\', "/"), text); }
    }
    Ok(values)
}

#[allow(clippy::too_many_arguments)]
fn parse_section(node: Node<'_, '_>, index: u32, source_path: &str, source: &DocumentSource, relationships: &BTreeMap<String, String>, headers: &BTreeMap<String, String>, footers: &BTreeMap<String, String>, previous: Option<&Section>) -> Result<Section, DocsightError> {
    let mut section = previous.cloned().unwrap_or_else(|| default_section(source, index));
    section.id = source.object_id("sect", source_path);
    section.section_index = index;
    if let Some(size) = child_element(node, "pgSz") {
        if let Some(value) = word_f32(size, "w") { section.page_width_pt = Some(value / 20.0); }
        if let Some(value) = word_f32(size, "h") { section.page_height_pt = Some(value / 20.0); }
    }
    if let Some(margins) = child_element(node, "pgMar") {
        if let Some(value) = word_f32(margins, "top") { section.margin_top_pt = Some(value / 20.0); }
        if let Some(value) = word_f32(margins, "right") { section.margin_right_pt = Some(value / 20.0); }
        if let Some(value) = word_f32(margins, "bottom") { section.margin_bottom_pt = Some(value / 20.0); }
        if let Some(value) = word_f32(margins, "left") { section.margin_left_pt = Some(value / 20.0); }
    }
    if let Some(value) = referenced_part_text(node, "headerReference", relationships, headers) { section.header_text = Some(value); }
    if let Some(value) = referenced_part_text(node, "footerReference", relationships, footers) { section.footer_text = Some(value); }
    validate_geometry(&section)?;
    Ok(section)
}

fn referenced_part_text(section: Node<'_, '_>, element: &str, relationships: &BTreeMap<String, String>, parts: &BTreeMap<String, String>) -> Option<String> {
    let reference = section.children().filter(|node| node.has_tag_name((W_NS, element))).find(|node| node.attribute((W_NS, "type")) == Some("default")).or_else(|| section.children().find(|node| node.has_tag_name((W_NS, element))))?;
    let id = reference.attribute((R_NS, "id"))?;
    parts.get(relationships.get(id)?).cloned()
}

fn validate_geometry(section: &Section) -> Result<(), DocsightError> {
    let width = section.page_width_pt.unwrap_or(612.0);
    let height = section.page_height_pt.unwrap_or(792.0);
    let left = section.margin_left_pt.unwrap_or(72.0);
    let right = section.margin_right_pt.unwrap_or(72.0);
    let top = section.margin_top_pt.unwrap_or(72.0);
    let bottom = section.margin_bottom_pt.unwrap_or(72.0);
    let values = [width, height, left, right, top, bottom];
    if values.iter().any(|value| !value.is_finite() || *value < 0.0) || width <= left + right || height <= top + bottom {
        return Err(DocsightError::MalformedDocument { message: "DOCX section geometry leaves no positive page content area".to_owned() });
    }
    Ok(())
}

fn default_section(source: &DocumentSource, index: u32) -> Section {
    Section {
        id: source.object_id("sect", &format!("/word/document.xml::layout-plan/default-section[{index}]")),
        section_index: index,
        page_width_pt: Some(612.0), page_height_pt: Some(792.0),
        margin_top_pt: Some(72.0), margin_right_pt: Some(72.0), margin_bottom_pt: Some(72.0), margin_left_pt: Some(72.0),
        header_text: None, footer_text: None,
    }
}

fn paragraph_block_count(node: Node<'_, '_>) -> usize {
    let mut segments = vec![String::new()];
    for descendant in node.descendants().filter(Node::is_element) {
        if descendant.ancestors().any(|ancestor| ancestor.has_tag_name((W_NS, "del"))) { continue; }
        if descendant.ancestors().any(|ancestor| {
            ancestor != descendant && ancestor.tag_name().name() == "fldSimple" && ancestor.attributes().any(|attribute| (attribute.name() == "instr" || attribute.name().ends_with(":instr")) && attribute.value().to_uppercase().contains("PAGE"))
        }) { continue; }
        if descendant.tag_name().name() == "fldSimple" {
            let is_page = descendant.attributes().any(|attribute| (attribute.name() == "instr" || attribute.name().ends_with(":instr")) && attribute.value().to_uppercase().contains("PAGE"));
            if is_page && let Some(last) = segments.last_mut() { last.push_str("[PAGE]"); }
            continue;
        }
        if descendant.has_tag_name((W_NS, "t")) {
            if let Some(value) = descendant.text() && let Some(last) = segments.last_mut() { last.push_str(value); }
        } else if descendant.has_tag_name((W_NS, "tab")) {
            if let Some(last) = segments.last_mut() { last.push('\t'); }
        } else if descendant.has_tag_name((W_NS, "br")) || descendant.has_tag_name((W_NS, "cr")) {
            let page_break = descendant.has_tag_name((W_NS, "br")) && descendant.attribute((W_NS, "type")).or_else(|| descendant.attribute("w:type")).is_some_and(|value| value == "page");
            if page_break {
                let leading = segments.len() == 1 && segments[0].trim().is_empty();
                if !leading { segments.push(String::new()); }
            } else if let Some(last) = segments.last_mut() { last.push('\n'); }
        }
    }
    if segments.len() > 1 && segments.last().is_some_and(|text| text.trim().is_empty()) { segments.pop(); }
    segments.len().max(1)
}

fn child_element<'a>(node: Node<'a, 'a>, local_name: &str) -> Option<Node<'a, 'a>> { node.children().find(|child| child.has_tag_name((W_NS, local_name))) }
fn word_f32(node: Node<'_, '_>, name: &str) -> Option<f32> { node.attribute((W_NS, name))?.parse::<f32>().ok() }
fn figure_count(paragraph: Node<'_, '_>) -> usize { paragraph.descendants().filter(|node| node.has_tag_name((W_NS, "drawing")) || node.has_tag_name((W_NS, "pict"))).count() }
fn count_error() -> DocsightError { DocsightError::ResourceLimit { resource: "DOCX layout plan blocks".to_owned(), limit: u32::MAX as u64 } }
