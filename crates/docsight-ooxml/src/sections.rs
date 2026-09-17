use crate::parser::{
    child_element, normalize_internal_target, on_off_value, parse_xml, twips_to_points, word_value,
};
use docsight_core::{
    Diagnostic, DiagnosticSeverity, DocsightError, DocumentSource, HeaderFooterKind,
    HeaderFooterVariant, ObjectId, Section, SectionHeaderFooter, SectionStart,
};
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub(crate) const PAGE_PLACEHOLDER: &str = "[PAGE]";
pub(crate) const NUMPAGES_PLACEHOLDER: &str = "[NUMPAGES]";
pub(crate) const SECTIONPAGES_PLACEHOLDER: &str = "[SECTIONPAGES]";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HeaderFooterPart {
    pub text: String,
    pub cached_fields: BTreeSet<String>,
}

pub(crate) struct SectionInputs<'a> {
    pub source: &'a DocumentSource,
    pub rels: &'a BTreeMap<String, (String, String)>,
    pub parts: &'a BTreeMap<String, HeaderFooterPart>,
    pub even_and_odd_headers: bool,
}

pub(crate) struct SectionLocation<'a> {
    pub index: u32,
    pub source_path: &'a str,
    pub last_block_id: Option<ObjectId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FieldPhase {
    Instruction,
    Result,
}

struct OpenField {
    instruction: String,
    phase: FieldPhase,
    placeholder: Option<&'static str>,
}

pub(crate) fn header_footer_parts(
    headers: &[(String, String)],
    footers: &[(String, String)],
) -> Result<BTreeMap<String, HeaderFooterPart>, DocsightError> {
    let mut parts = BTreeMap::new();
    for (name, xml) in headers.iter().chain(footers) {
        parts.insert(name.clone(), header_footer_part(xml)?);
    }
    Ok(parts)
}

pub(crate) fn even_and_odd_headers(settings: Option<&str>) -> Result<bool, DocsightError> {
    let Some(xml) = settings else {
        return Ok(false);
    };
    let document = parse_xml(xml)?;
    Ok(child_element(document.root_element(), "evenAndOddHeaders")
        .is_some_and(|flag| on_off_value(word_value(flag).as_deref())))
}

fn header_footer_part(xml: &str) -> Result<HeaderFooterPart, DocsightError> {
    let document = parse_xml(xml)?;
    let mut fields: Vec<OpenField> = Vec::new();
    let mut cached_fields = BTreeSet::new();
    let mut paragraphs = Vec::new();
    for paragraph in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "p")))
    {
        let mut text = String::new();
        for node in paragraph
            .descendants()
            .filter(|node| node.is_element() && owning_paragraph(*node) == Some(paragraph))
        {
            if is_deleted(node, paragraph) || inside_placeholder_simple_field(node, paragraph) {
                continue;
            }
            match node.tag_name().name() {
                "fldSimple" if node.tag_name().namespace() == Some(W_NS) => {
                    let instruction = node.attribute((W_NS, "instr")).unwrap_or_default();
                    match field_placeholder(instruction) {
                        Some(placeholder) => text.push_str(placeholder),
                        None => record_cached_field(instruction, &mut cached_fields),
                    }
                }
                "fldChar" if node.tag_name().namespace() == Some(W_NS) => {
                    handle_field_character(node, &mut fields, &mut cached_fields, &mut text)?;
                }
                "instrText" if node.tag_name().namespace() == Some(W_NS) => {
                    if let Some(field) = fields.last_mut()
                        && field.phase == FieldPhase::Instruction
                    {
                        field.instruction.push_str(node.text().unwrap_or_default());
                    }
                }
                "t" if node.tag_name().namespace() == Some(W_NS) && !suppresses_text(&fields) => {
                    text.push_str(node.text().unwrap_or_default());
                }
                "tab" if node.tag_name().namespace() == Some(W_NS) && !suppresses_text(&fields) => {
                    text.push('\t');
                }
                "br" | "cr"
                    if node.tag_name().namespace() == Some(W_NS) && !suppresses_text(&fields) =>
                {
                    text.push('\n');
                }
                _ => {}
            }
        }
        paragraphs.push(text);
    }
    if !fields.is_empty() {
        return Err(DocsightError::MalformedDocument {
            message: "header or footer contains an unterminated field".to_owned(),
        });
    }
    Ok(HeaderFooterPart {
        text: paragraphs.join("\n"),
        cached_fields,
    })
}

fn handle_field_character(
    node: Node<'_, '_>,
    fields: &mut Vec<OpenField>,
    cached_fields: &mut BTreeSet<String>,
    text: &mut String,
) -> Result<(), DocsightError> {
    match node.attribute((W_NS, "fldCharType")) {
        Some("begin") => fields.push(OpenField {
            instruction: String::new(),
            phase: FieldPhase::Instruction,
            placeholder: None,
        }),
        Some("separate") => {
            if let Some(field) = fields.last_mut()
                && field.phase == FieldPhase::Instruction
            {
                field.phase = FieldPhase::Result;
                field.placeholder = field_placeholder(&field.instruction);
                match field.placeholder {
                    Some(placeholder) => text.push_str(placeholder),
                    None => record_cached_field(&field.instruction, cached_fields),
                }
            }
        }
        Some("end") => {
            if let Some(field) = fields.pop()
                && field.phase == FieldPhase::Instruction
            {
                match field_placeholder(&field.instruction) {
                    Some(placeholder) => text.push_str(placeholder),
                    None => record_cached_field(&field.instruction, cached_fields),
                }
            }
        }
        Some(other) => {
            return Err(DocsightError::MalformedDocument {
                message: format!("header or footer field character type {other} is not defined"),
            });
        }
        None => {
            return Err(DocsightError::MalformedDocument {
                message: "header or footer field character has no fldCharType".to_owned(),
            });
        }
    }
    Ok(())
}

fn suppresses_text(fields: &[OpenField]) -> bool {
    fields.iter().any(|field| {
        field.phase == FieldPhase::Instruction
            || (field.phase == FieldPhase::Result && field.placeholder.is_some())
    })
}

fn field_placeholder(instruction: &str) -> Option<&'static str> {
    let keyword = instruction.split_whitespace().next()?.to_ascii_uppercase();
    match keyword.as_str() {
        "PAGE" => Some(PAGE_PLACEHOLDER),
        "NUMPAGES" => Some(NUMPAGES_PLACEHOLDER),
        "SECTIONPAGES" => Some(SECTIONPAGES_PLACEHOLDER),
        _ => None,
    }
}

fn record_cached_field(instruction: &str, cached_fields: &mut BTreeSet<String>) {
    if let Some(keyword) = instruction.split_whitespace().next() {
        cached_fields.insert(keyword.to_ascii_uppercase());
    }
}

fn owning_paragraph<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    node.ancestors()
        .find(|ancestor| ancestor.has_tag_name((W_NS, "p")))
}

fn is_deleted(node: Node<'_, '_>, paragraph: Node<'_, '_>) -> bool {
    node.ancestors()
        .take_while(|ancestor| *ancestor != paragraph)
        .any(|ancestor| ancestor.has_tag_name((W_NS, "del")))
}

fn inside_placeholder_simple_field(node: Node<'_, '_>, paragraph: Node<'_, '_>) -> bool {
    node.ancestors()
        .skip(1)
        .take_while(|ancestor| *ancestor != paragraph)
        .any(|ancestor| {
            ancestor.has_tag_name((W_NS, "fldSimple"))
                && field_placeholder(ancestor.attribute((W_NS, "instr")).unwrap_or_default())
                    .is_some()
        })
}

pub(crate) fn parse_section(
    node: Node<'_, '_>,
    location: SectionLocation<'_>,
    previous: Option<&Section>,
    inputs: &SectionInputs<'_>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Section, DocsightError> {
    let id = inputs.source.object_id("sect", location.source_path);
    let mut section = Section {
        id: id.clone(),
        section_index: location.index,
        page_width_pt: None,
        page_height_pt: None,
        margin_top_pt: None,
        margin_right_pt: None,
        margin_bottom_pt: None,
        margin_left_pt: None,
        header_text: None,
        footer_text: None,
        start: section_start(node)?,
        title_page: child_element(node, "titlePg")
            .is_some_and(|flag| on_off_value(word_value(flag).as_deref())),
        even_and_odd_headers: inputs.even_and_odd_headers,
        header_distance_pt: None,
        footer_distance_pt: None,
        columns: section_columns(node)?,
        page_number_start: None,
        page_number_format: None,
        headers_footers: Vec::new(),
        last_block_id: location.last_block_id,
    };
    let mut missing = Vec::new();
    read_page_size(node, &mut section, &mut missing)?;
    read_page_margins(node, &mut section, &mut missing, warnings)?;
    read_page_numbering(node, &mut section)?;
    section.headers_footers = section_headers_footers(node, &id, previous, inputs, warnings)?;
    section.header_text = default_text(&section, HeaderFooterKind::Header);
    section.footer_text = default_text(&section, HeaderFooterKind::Footer);
    if section
        .headers_footers
        .iter()
        .any(|entry| !entry.text.trim().is_empty())
    {
        if section.header_distance_pt.is_none() {
            missing.push("pgMar/@header");
        }
        if section.footer_distance_pt.is_none() {
            missing.push("pgMar/@footer");
        }
    }
    if !missing.is_empty() {
        warnings.push(section_warning(
            "DOCX_SECTION_GEOMETRY_DEFAULTED",
            &id,
            format!(
                "section {} omits {}",
                location.index,
                missing.join(", ")
            ),
            "omitted page size uses Letter 612x792 pt, omitted margins use 72 pt and omitted header or footer distances use 36 pt",
        ));
    }
    if section.columns > 1 {
        warnings.push(section_warning(
            "DOCX_SECTION_COLUMNS_UNSUPPORTED",
            &id,
            format!(
                "section {} declares {} text columns",
                location.index, section.columns
            ),
            "the section is laid out as a single column, so line breaks, pagination and geometry differ from Word",
        ));
    }
    Ok(section)
}

pub(crate) fn defaulted_section(
    source: &DocumentSource,
    index: u32,
    source_path: &str,
    last_block_id: Option<ObjectId>,
    even_and_odd_headers: bool,
) -> Section {
    Section {
        id: source.object_id("sect", source_path),
        section_index: index,
        page_width_pt: Some(612.0),
        page_height_pt: Some(792.0),
        margin_top_pt: Some(72.0),
        margin_right_pt: Some(72.0),
        margin_bottom_pt: Some(72.0),
        margin_left_pt: Some(72.0),
        header_text: None,
        footer_text: None,
        start: SectionStart::NextPage,
        title_page: false,
        even_and_odd_headers,
        header_distance_pt: None,
        footer_distance_pt: None,
        columns: 1,
        page_number_start: None,
        page_number_format: None,
        headers_footers: Vec::new(),
        last_block_id,
    }
}

fn section_start(node: Node<'_, '_>) -> Result<SectionStart, DocsightError> {
    let Some(value) = child_element(node, "type").and_then(word_value) else {
        return Ok(SectionStart::NextPage);
    };
    match value.as_str() {
        "nextPage" => Ok(SectionStart::NextPage),
        "continuous" => Ok(SectionStart::Continuous),
        "evenPage" => Ok(SectionStart::EvenPage),
        "oddPage" => Ok(SectionStart::OddPage),
        "nextColumn" => Ok(SectionStart::NextColumn),
        other => Err(DocsightError::MalformedDocument {
            message: format!("section break type {other} is not defined"),
        }),
    }
}

fn section_columns(node: Node<'_, '_>) -> Result<u32, DocsightError> {
    let Some(value) = child_element(node, "cols").and_then(|cols| cols.attribute((W_NS, "num")))
    else {
        return Ok(1);
    };
    match value.trim().parse::<u32>() {
        Ok(columns) if columns >= 1 => Ok(columns),
        _ => Err(DocsightError::MalformedDocument {
            message: format!("section column count {value} is not a positive integer"),
        }),
    }
}

fn read_page_size(
    node: Node<'_, '_>,
    section: &mut Section,
    missing: &mut Vec<&'static str>,
) -> Result<(), DocsightError> {
    let size = child_element(node, "pgSz");
    section.page_width_pt = optional_points(size, "w", "section page width")?;
    section.page_height_pt = optional_points(size, "h", "section page height")?;
    if section.page_width_pt.is_none() {
        missing.push("pgSz/@w");
    }
    if section.page_height_pt.is_none() {
        missing.push("pgSz/@h");
    }
    Ok(())
}

fn read_page_margins(
    node: Node<'_, '_>,
    section: &mut Section,
    missing: &mut Vec<&'static str>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(), DocsightError> {
    let margins = child_element(node, "pgMar");
    section.margin_top_pt = optional_points(margins, "top", "section top margin")?.map(f32::abs);
    section.margin_right_pt = optional_points(margins, "right", "section right margin")?;
    section.margin_bottom_pt =
        optional_points(margins, "bottom", "section bottom margin")?.map(f32::abs);
    section.margin_left_pt = optional_points(margins, "left", "section left margin")?;
    section.header_distance_pt = optional_points(margins, "header", "section header distance")?;
    section.footer_distance_pt = optional_points(margins, "footer", "section footer distance")?;
    for (value, name) in [
        (section.margin_top_pt, "pgMar/@top"),
        (section.margin_right_pt, "pgMar/@right"),
        (section.margin_bottom_pt, "pgMar/@bottom"),
        (section.margin_left_pt, "pgMar/@left"),
    ] {
        if value.is_none() {
            missing.push(name);
        }
    }
    if let Some(gutter) = optional_points(margins, "gutter", "section gutter")?
        && gutter > 0.0
    {
        warnings.push(section_warning(
            "DOCX_SECTION_GUTTER_IGNORED",
            &section.id,
            format!(
                "section {} declares a {gutter} pt binding gutter",
                section.section_index
            ),
            "the gutter is not added to the page margins, so the text area is wider than in Word",
        ));
    }
    Ok(())
}

fn read_page_numbering(node: Node<'_, '_>, section: &mut Section) -> Result<(), DocsightError> {
    let Some(numbering) = child_element(node, "pgNumType") else {
        return Ok(());
    };
    if let Some(value) = numbering.attribute((W_NS, "start")) {
        section.page_number_start =
            Some(
                value
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| DocsightError::MalformedDocument {
                        message: format!("section page number start {value} is not an integer"),
                    })?,
            );
    }
    section.page_number_format = numbering.attribute((W_NS, "fmt")).map(str::to_owned);
    Ok(())
}

fn optional_points(
    node: Option<Node<'_, '_>>,
    attribute: &str,
    field: &str,
) -> Result<Option<f32>, DocsightError> {
    node.and_then(|node| node.attribute((W_NS, attribute)))
        .map(|value| twips_to_points(value, field))
        .transpose()
}

fn section_headers_footers(
    node: Node<'_, '_>,
    section_id: &ObjectId,
    previous: Option<&Section>,
    inputs: &SectionInputs<'_>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Vec<SectionHeaderFooter>, DocsightError> {
    let mut entries: BTreeMap<(HeaderFooterKind, HeaderFooterVariant), SectionHeaderFooter> =
        BTreeMap::new();
    for reference in node.children().filter(Node::is_element) {
        let kind = if reference.has_tag_name((W_NS, "headerReference")) {
            HeaderFooterKind::Header
        } else if reference.has_tag_name((W_NS, "footerReference")) {
            HeaderFooterKind::Footer
        } else {
            continue;
        };
        let variant = reference_variant(reference)?;
        let Some(part) = resolve_reference(reference, inputs) else {
            warnings.push(section_warning(
                "DOCX_HEADER_FOOTER_UNRESOLVED",
                section_id,
                format!(
                    "a {} reference in section {} does not resolve to a package part",
                    kind_name(kind),
                    section_id
                ),
                "the header or footer content of this reference is unavailable and is not projected onto pages",
            ));
            continue;
        };
        let (part_name, content) = part;
        if !content.cached_fields.is_empty() {
            warnings.push(section_warning(
                "DOCX_HEADER_FOOTER_FIELD_CACHED",
                section_id,
                format!(
                    "/{part_name} contains fields that are not recomputed: {}",
                    content
                        .cached_fields
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "those fields show the result cached in the document, which may be stale on each page",
            ));
        }
        let replaced = entries.insert(
            (kind, variant),
            SectionHeaderFooter {
                kind,
                variant,
                part: format!("/{part_name}"),
                text: content.text.clone(),
                inherited: false,
            },
        );
        if replaced.is_some() {
            return Err(DocsightError::MalformedDocument {
                message: format!(
                    "section {section_id} declares duplicate {} {} references",
                    kind_name(kind),
                    variant_name(variant)
                ),
            });
        }
    }
    if let Some(previous) = previous {
        for inherited in &previous.headers_footers {
            entries
                .entry((inherited.kind, inherited.variant))
                .or_insert_with(|| SectionHeaderFooter {
                    inherited: true,
                    ..inherited.clone()
                });
        }
    }
    Ok(entries.into_values().collect())
}

fn reference_variant(reference: Node<'_, '_>) -> Result<HeaderFooterVariant, DocsightError> {
    match reference.attribute((W_NS, "type")) {
        None | Some("default") => Ok(HeaderFooterVariant::Default),
        Some("first") => Ok(HeaderFooterVariant::First),
        Some("even") => Ok(HeaderFooterVariant::Even),
        Some(other) => Err(DocsightError::MalformedDocument {
            message: format!("header or footer reference type {other} is not defined"),
        }),
    }
}

fn resolve_reference<'a>(
    reference: Node<'_, '_>,
    inputs: &'a SectionInputs<'_>,
) -> Option<(&'a String, &'a HeaderFooterPart)> {
    let relationship = reference.attribute((R_NS, "id"))?;
    let (_, target) = inputs.rels.get(relationship)?;
    let name = normalize_internal_target(target)?;
    inputs.parts.get_key_value(&name)
}

fn default_text(section: &Section, kind: HeaderFooterKind) -> Option<String> {
    section
        .header_footer(kind, HeaderFooterVariant::Default)
        .map(|entry| entry.text.clone())
        .filter(|text| !text.trim().is_empty())
}

fn kind_name(kind: HeaderFooterKind) -> &'static str {
    match kind {
        HeaderFooterKind::Header => "header",
        HeaderFooterKind::Footer => "footer",
    }
}

fn variant_name(variant: HeaderFooterVariant) -> &'static str {
    match variant {
        HeaderFooterVariant::Default => "default",
        HeaderFooterVariant::First => "first",
        HeaderFooterVariant::Even => "even",
    }
}

fn section_warning(code: &str, section_id: &ObjectId, message: String, effect: &str) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Warning,
        message,
        effect: effect.to_owned(),
        object: Some(section_id.clone()),
        page: None,
    }
}
