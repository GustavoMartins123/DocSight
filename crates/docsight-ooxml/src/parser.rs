use crate::model::{
    DocxBlock, DocxDocument, ListMarker, Paragraph, ParagraphKind, Table, TableCell,
};
use crate::package::read_parts;
use docsight_core::{
    Diagnostic, DiagnosticSeverity, DocsightError, DocumentFormat, DocumentSource,
};
use roxmltree::{Document as XmlDocument, Node};
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
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

pub fn parse_docx(source: &DocumentSource) -> Result<DocxDocument, DocsightError> {
    if source.format() != DocumentFormat::Docx {
        return Err(DocsightError::UnsupportedOperation {
            operation: "DOCX structural parsing".to_owned(),
            format: source.format(),
        });
    }
    let parts = read_parts(source.bytes())?;
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
    let mut warnings = Vec::new();
    let mut paragraph_index = 0_u32;
    let mut table_index = 0_u32;
    for child in body.children().filter(Node::is_element) {
        if child.has_tag_name((W_NS, "p")) {
            paragraph_index = paragraph_index
                .checked_add(1)
                .ok_or_else(block_count_error)?;
            blocks.push(DocxBlock::Paragraph(parse_paragraph(
                child,
                paragraph_index,
                source,
                &styles,
                &numbering,
                &mut warnings,
            )?));
        } else if child.has_tag_name((W_NS, "tbl")) {
            table_index = table_index.checked_add(1).ok_or_else(block_count_error)?;
            blocks.push(DocxBlock::Table(parse_table(child, table_index, source)?));
        } else if !child.has_tag_name((W_NS, "sectPr")) {
            warnings.push(Diagnostic {
                code: "DOCX_BODY_ELEMENT_UNSUPPORTED".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!("unsupported body element: {}", child.tag_name().name()),
                effect: "the element is omitted from structural output".to_owned(),
            });
        }
    }
    Ok(DocxDocument { blocks, warnings })
}

fn parse_paragraph(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
    styles: &BTreeMap<String, StyleDefinition>,
    numbering: &NumberingDefinitions,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Paragraph, DocsightError> {
    let source_path = format!("/word/document.xml::body/p[{index}]");
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
    let kind = if heading_level.is_some() {
        ParagraphKind::Heading
    } else if list.is_some() {
        ParagraphKind::ListItem
    } else {
        ParagraphKind::Paragraph
    };
    let prefix = match kind {
        ParagraphKind::Heading => "h",
        ParagraphKind::ListItem => "li",
        ParagraphKind::Paragraph => "p",
    };
    Ok(Paragraph {
        id: source.object_id(prefix, &source_path),
        text: paragraph_text(node),
        kind,
        style_id,
        heading_level,
        list,
        source: source_path,
    })
}

fn parse_table(
    node: Node<'_, '_>,
    index: u32,
    source: &DocumentSource,
) -> Result<Table, DocsightError> {
    let source_path = format!("/word/document.xml::body/tbl[{index}]");
    parse_table_at(node, source_path, source, 0)
}

fn parse_table_at(
    node: Node<'_, '_>,
    source_path: String,
    source: &DocumentSource,
    depth: usize,
) -> Result<Table, DocsightError> {
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
        for cell_node in row_node
            .children()
            .filter(|child| child.has_tag_name((W_NS, "tc")))
        {
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
                    parse_table_at(nested, nested_source, source, depth + 1)
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
                        cells[cell_index].nested_tables.extend(nested_tables);
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
                        text,
                        nested_tables,
                        source: cell_source,
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
        columns = columns.max(column);
        active_merges = next_merges;
    }
    Ok(Table {
        id: source.object_id("tbl", &source_path),
        rows,
        columns,
        cells,
        source: source_path,
    })
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

fn resolve_style_numbering(
    style_id: Option<&str>,
    styles: &BTreeMap<String, StyleDefinition>,
) -> Result<Option<NumberingProperties>, DocsightError> {
    let Some(mut current) = style_id else {
        return Ok(None);
    };
    let mut visited = BTreeSet::new();
    let mut resolved = NumberingProperties::default();
    for _ in 0..MAX_STYLE_DEPTH {
        if !visited.insert(current.to_owned()) {
            return Err(DocsightError::MalformedDocument {
                message: format!("cycle detected in paragraph style inheritance: {current}"),
            });
        }
        let Some(style) = styles.get(current) else {
            return Ok(None);
        };
        if let Some(numbering) = &style.numbering {
            if resolved.num_id.is_none() {
                resolved.num_id.clone_from(&numbering.num_id);
            }
            if resolved.level.is_none() {
                resolved.level = numbering.level;
            }
        }
        let Some(parent) = style.based_on.as_deref() else {
            return Ok((resolved.num_id.is_some() || resolved.level.is_some()).then_some(resolved));
        };
        current = parent;
    }
    Err(DocsightError::MalformedDocument {
        message: format!("paragraph style inheritance exceeds {MAX_STYLE_DEPTH} levels"),
    })
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

fn parse_numbering(xml: Option<&str>) -> Result<NumberingDefinitions, DocsightError> {
    let Some(xml) = xml else {
        return Ok(NumberingDefinitions::default());
    };
    let document = XmlDocument::parse(xml).map_err(xml_error)?;
    let mut definitions = NumberingDefinitions::default();
    for abstract_node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "abstractNum")))
    {
        let abstract_id = abstract_node
            .attribute((W_NS, "abstractNumId"))
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: "abstract numbering definition has no abstractNumId".to_owned(),
            })?;
        for level_node in abstract_node
            .children()
            .filter(|node| node.has_tag_name((W_NS, "lvl")))
        {
            let level_value = level_node.attribute((W_NS, "ilvl")).ok_or_else(|| {
                DocsightError::MalformedDocument {
                    message: "numbering level has no ilvl".to_owned(),
                }
            })?;
            let level =
                level_value
                    .parse::<u8>()
                    .map_err(|error| DocsightError::MalformedDocument {
                        message: format!("invalid numbering level: {error}"),
                    })?;
            let format = child_element(level_node, "numFmt").and_then(word_value);
            let pattern = child_element(level_node, "lvlText").and_then(word_value);
            definitions.levels.insert(
                (abstract_id.to_owned(), level),
                NumberingLevel { format, pattern },
            );
        }
    }
    for number_node in document
        .descendants()
        .filter(|node| node.has_tag_name((W_NS, "num")))
    {
        let num_id = number_node.attribute((W_NS, "numId")).ok_or_else(|| {
            DocsightError::MalformedDocument {
                message: "numbering instance has no numId".to_owned(),
            }
        })?;
        let abstract_id = child_element(number_node, "abstractNumId")
            .and_then(word_value)
            .ok_or_else(|| DocsightError::MalformedDocument {
                message: format!("numbering instance {num_id} has no abstractNumId"),
            })?;
        definitions.numbers.insert(num_id.to_owned(), abstract_id);
    }
    Ok(definitions)
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
                warnings.push(Diagnostic {
                    code: "DOCX_NUMBERING_FORMAT_MISSING".to_owned(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("numbering format is missing for numId {num_id}"),
                    effect: "the list item ordering semantics are unavailable".to_owned(),
                });
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
            warnings.push(Diagnostic {
                code: "DOCX_NUMBERING_UNRESOLVED".to_owned(),
                severity: DiagnosticSeverity::Warning,
                message: format!("numbering definition was not resolved for numId {num_id}"),
                effect: "the list item marker format is unknown".to_owned(),
            });
            (None, None, None)
        }
    };
    Some(ListMarker {
        num_id,
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
