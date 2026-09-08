use crate::syntax::{Value, malformed};
use docsight_core::{DocsightError, Rect};
use std::collections::{BTreeMap, BTreeSet};

const MAX_OPERATIONS: usize = 1_000_000;
const MAX_GRAPHICS_DEPTH: usize = 64;
const MAX_PATH_SEGMENTS: usize = 100_000;
const MAX_OPERANDS: usize = 100_000;
const MAX_STRING_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PathSegment {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipRegion {
    pub polygons: Vec<Vec<Point>>,
    pub even_odd: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextRun {
    pub text: String,
    pub bbox: Rect,
    pub baseline_y: f32,
    pub font_size: f32,
    pub font_name: String,
    pub bold: bool,
    pub argb: u32,
    pub source_offset: u64,
    pub source_length: u64,
    pub clips: Vec<ClipRegion>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DisplayCommand {
    Text(TextRun),
    Figure {
        bbox: Rect,
        resource_name: String,
        clips: Vec<ClipRegion>,
    },
    Fill {
        path: Vec<PathSegment>,
        color: Color,
        even_odd: bool,
        clips: Vec<ClipRegion>,
    },
    Stroke {
        path: Vec<PathSegment>,
        color: Color,
        width: f32,
        clips: Vec<ClipRegion>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct FontInfo {
    pub bold: bool,
    pub base_font: String,
    decoder: FontDecoder,
}

#[derive(Clone, Debug)]
enum FontDecoder {
    Ascii,
    WinAnsi,
    ToUnicode(ToUnicodeMap),
}

#[derive(Clone, Debug)]
struct ToUnicodeMap {
    mappings: BTreeMap<Vec<u8>, String>,
    code_lengths: Vec<usize>,
}

pub(crate) struct ParsedContent {
    pub commands: Vec<DisplayCommand>,
    pub text_runs: Vec<TextRun>,
    pub approximated_font: bool,
    pub approximated_graphics: bool,
    pub omitted_xobjects: bool,
}

pub(crate) fn parse_content(
    bytes: &[u8],
    page_left: f32,
    page_height: f32,
    fonts: &BTreeMap<String, FontInfo>,
    xobjects: &BTreeSet<String>,
) -> Result<ParsedContent, DocsightError> {
    let mut lexer = ContentLexer::new(bytes);
    let mut operands = Vec::new();
    let mut state = GraphicsState::default();
    let mut stack = Vec::new();
    let mut path = Vec::new();
    let mut commands = Vec::new();
    let mut text_runs = Vec::new();
    let mut in_text = false;
    let mut marked_content_depth = 0usize;
    let mut operations = 0usize;
    let mut approximated_font = false;
    let mut approximated_graphics = false;
    let mut omitted_xobjects = false;
    let mut inline_image = false;
    while let Some(token) = lexer.next_token()? {
        match token {
            ContentToken::Operand(value) => {
                if operands.len() >= MAX_OPERANDS {
                    return Err(DocsightError::ResourceLimit {
                        resource: "PDF content operands".to_owned(),
                        limit: MAX_OPERANDS as u64,
                    });
                }
                operands.push(value);
            }
            ContentToken::Operator(operator, token_start, token_end) => {
                operations += 1;
                if operations > MAX_OPERATIONS {
                    return Err(DocsightError::ResourceLimit {
                        resource: "PDF content operations".to_owned(),
                        limit: MAX_OPERATIONS as u64,
                    });
                }
                let anchor_offset = token_start as u64;
                let anchor_length = (token_end - token_start) as u64;
                match operator.as_str() {
                    "BX" | "EX" => {
                        require_empty(&operands, &operator)?;
                    }
                    "BI" => {
                        require_empty(&operands, &operator)?;
                        inline_image = true;
                        approximated_graphics = true;
                    }
                    "ID" => {
                        if !inline_image {
                            return Err(DocsightError::UnsupportedFeature {
                                feature: "PDF inline image data marker outside BI/ID/EI".to_owned(),
                            });
                        }
                        validate_inline_image_dictionary(&operands)?;
                        commands.push(DisplayCommand::Figure {
                            bbox: transformed_unit_bbox(&state.ctm, page_left, page_height)?,
                            resource_name: "<inline-image>".to_owned(),
                            clips: state.clips.clone(),
                        });
                        operands.clear();
                        lexer.skip_inline_image()?;
                        inline_image = false;
                        omitted_xobjects = true;
                    }
                    "q" => {
                        require_empty(&operands, &operator)?;
                        if stack.len() >= MAX_GRAPHICS_DEPTH {
                            return Err(DocsightError::ResourceLimit {
                                resource: "PDF graphics state depth".to_owned(),
                                limit: MAX_GRAPHICS_DEPTH as u64,
                            });
                        }
                        stack.push(state.clone());
                    }
                    "Q" => {
                        require_empty(&operands, &operator)?;
                        state = stack
                            .pop()
                            .ok_or_else(|| malformed("unbalanced Q operator"))?;
                    }
                    "cm" => {
                        let values = numbers(&operands, 6, &operator)?;
                        state.ctm = Matrix::new(
                            values[0], values[1], values[2], values[3], values[4], values[5],
                        )
                        .concat(state.ctm);
                        if !state.ctm.is_finite() {
                            return Err(malformed("cm operator produced a non-finite matrix"));
                        }
                    }
                    "w" => {
                        let values = numbers(&operands, 1, &operator)?;
                        if values[0] < 0.0 {
                            return Err(malformed("line width must be non-negative"));
                        }
                        if values[0] == 0.0 {
                            approximated_graphics = true;
                        }
                        state.line_width = values[0];
                    }
                    "rg" => {
                        let values = numbers(&operands, 3, &operator)?;
                        state.fill_color = rgb(values[0], values[1], values[2])?;
                    }
                    "RG" => {
                        let values = numbers(&operands, 3, &operator)?;
                        state.stroke_color = rgb(values[0], values[1], values[2])?;
                    }
                    "g" => {
                        let values = numbers(&operands, 1, &operator)?;
                        state.fill_color = rgb(values[0], values[0], values[0])?;
                    }
                    "G" => {
                        let values = numbers(&operands, 1, &operator)?;
                        state.stroke_color = rgb(values[0], values[0], values[0])?;
                    }
                    "k" => {
                        let values = numbers(&operands, 4, &operator)?;
                        state.fill_color = cmyk(values[0], values[1], values[2], values[3])?;
                    }
                    "K" => {
                        let values = numbers(&operands, 4, &operator)?;
                        state.stroke_color = cmyk(values[0], values[1], values[2], values[3])?;
                    }
                    "cs" | "CS" => {
                        if operands.len() != 1 {
                            return Err(malformed(format!(
                                "{operator} requires one color-space name"
                            )));
                        }
                        name(&operands[0], &operator)?;
                        approximated_graphics = true;
                    }
                    "sc" | "SC" | "scn" | "SCN" => {
                        if operands
                            .iter()
                            .any(|value| matches!(value, ContentValue::Name(_)))
                        {
                            approximated_graphics = true;
                        }
                        let values = operands
                            .iter()
                            .filter(|value| matches!(value, ContentValue::Number(_)))
                            .map(|value| number(value, &operator))
                            .collect::<Result<Vec<_>, _>>()?;
                        if values.is_empty() || values.len() > 4 {
                            if matches!(operator.as_str(), "scn" | "SCN")
                                && operands
                                    .iter()
                                    .any(|value| matches!(value, ContentValue::Name(_)))
                            {
                                approximated_graphics = true;
                            } else {
                                return Err(malformed(format!(
                                    "{operator} requires one to four color components"
                                )));
                            }
                        } else {
                            let color = color_components(&values, &operator)?;
                            if matches!(operator.as_str(), "sc" | "scn") {
                                state.fill_color = color;
                            } else {
                                state.stroke_color = color;
                            }
                        }
                        if operator == "scn" || operator == "SCN" {
                            approximated_graphics = true;
                        }
                    }
                    "m" => {
                        let values = numbers(&operands, 2, &operator)?;
                        path.push(PathSegment::Move(public_point(
                            state.ctm.transform(values[0], values[1]),
                            page_left,
                            page_height,
                        )?));
                    }
                    "l" => {
                        let values = numbers(&operands, 2, &operator)?;
                        require_open_subpath(&path, &operator)?;
                        path.push(PathSegment::Line(public_point(
                            state.ctm.transform(values[0], values[1]),
                            page_left,
                            page_height,
                        )?));
                    }
                    "c" => {
                        let values = numbers(&operands, 6, &operator)?;
                        require_open_subpath(&path, &operator)?;
                        path.push(PathSegment::Cubic(
                            public_point(
                                state.ctm.transform(values[0], values[1]),
                                page_left,
                                page_height,
                            )?,
                            public_point(
                                state.ctm.transform(values[2], values[3]),
                                page_left,
                                page_height,
                            )?,
                            public_point(
                                state.ctm.transform(values[4], values[5]),
                                page_left,
                                page_height,
                            )?,
                        ));
                    }
                    "v" => {
                        let values = numbers(&operands, 4, &operator)?;
                        let first = current_path_point(&path)?;
                        require_open_subpath(&path, &operator)?;
                        path.push(PathSegment::Cubic(
                            first,
                            public_point(
                                state.ctm.transform(values[0], values[1]),
                                page_left,
                                page_height,
                            )?,
                            public_point(
                                state.ctm.transform(values[2], values[3]),
                                page_left,
                                page_height,
                            )?,
                        ));
                    }
                    "y" => {
                        let values = numbers(&operands, 4, &operator)?;
                        let end = public_point(
                            state.ctm.transform(values[2], values[3]),
                            page_left,
                            page_height,
                        )?;
                        require_open_subpath(&path, &operator)?;
                        path.push(PathSegment::Cubic(
                            public_point(
                                state.ctm.transform(values[0], values[1]),
                                page_left,
                                page_height,
                            )?,
                            end,
                            end,
                        ));
                    }
                    "re" => {
                        let values = numbers(&operands, 4, &operator)?;
                        append_rectangle(&mut path, &state.ctm, page_left, page_height, &values)?;
                    }
                    "h" => {
                        require_empty(&operands, &operator)?;
                        require_open_subpath(&path, &operator)?;
                        path.push(PathSegment::Close);
                    }
                    "S" | "s" => {
                        require_empty(&operands, &operator)?;
                        if operator == "s" {
                            path.push(PathSegment::Close);
                        }
                        if path.is_empty() {
                            return Err(malformed("stroke operator has no current path"));
                        }
                        let width = state.ctm.stroke_width(state.line_width)?;
                        commit_pending_clip(&mut state)?;
                        commands.push(DisplayCommand::Stroke {
                            path: std::mem::take(&mut path),
                            color: state.stroke_color,
                            width,
                            clips: state.clips.clone(),
                        });
                    }
                    "f" | "F" | "f*" => {
                        require_empty(&operands, &operator)?;
                        if path.is_empty() {
                            return Err(malformed("fill operator has no current path"));
                        }
                        commit_pending_clip(&mut state)?;
                        commands.push(DisplayCommand::Fill {
                            path: std::mem::take(&mut path),
                            color: state.fill_color,
                            even_odd: operator == "f*",
                            clips: state.clips.clone(),
                        });
                    }
                    "B" | "B*" | "b" | "b*" => {
                        require_empty(&operands, &operator)?;
                        if path.is_empty() {
                            return Err(malformed(
                                "combined fill and stroke operator has no current path",
                            ));
                        }
                        if matches!(operator.as_str(), "b" | "b*") {
                            path.push(PathSegment::Close);
                        }
                        commit_pending_clip(&mut state)?;
                        let even_odd = matches!(operator.as_str(), "B*" | "b*");
                        let stroke_path = path.clone();
                        let width = state.ctm.stroke_width(state.line_width)?;
                        commands.push(DisplayCommand::Fill {
                            path,
                            color: state.fill_color,
                            even_odd,
                            clips: state.clips.clone(),
                        });
                        commands.push(DisplayCommand::Stroke {
                            path: stroke_path,
                            color: state.stroke_color,
                            width,
                            clips: state.clips.clone(),
                        });
                        path = Vec::new();
                    }
                    "n" => {
                        require_empty(&operands, &operator)?;
                        commit_pending_clip(&mut state)?;
                        path.clear();
                    }
                    "BT" => {
                        require_empty(&operands, &operator)?;
                        if in_text {
                            return Err(malformed("nested BT operator"));
                        }
                        in_text = true;
                        state.text.matrix = Matrix::identity();
                        state.text.line_matrix = Matrix::identity();
                    }
                    "ET" => {
                        require_empty(&operands, &operator)?;
                        if !in_text {
                            return Err(malformed("ET operator outside a text object"));
                        }
                        in_text = false;
                    }
                    "Tf" => {
                        if operands.len() != 2 {
                            return Err(malformed("Tf requires a font name and size"));
                        }
                        let name = name(&operands[0], &operator)?;
                        let size = number(&operands[1], &operator)?;
                        if size <= 0.0 || !size.is_finite() {
                            return Err(malformed("font size must be positive and finite"));
                        }
                        let font = fonts
                            .get(name)
                            .ok_or_else(|| malformed("Tf references an unknown font resource"))?;
                        state.text.font = Some(FontSelection {
                            size,
                            bold: font.bold,
                            font_name: font.base_font.clone(),
                            decoder: font.decoder.clone(),
                        });
                        approximated_font = true;
                    }
                    "Tm" => {
                        require_text(in_text, &operator)?;
                        let values = numbers(&operands, 6, &operator)?;
                        let matrix = Matrix::new(
                            values[0], values[1], values[2], values[3], values[4], values[5],
                        );
                        validate_text_matrix(&matrix)?;
                        state.text.matrix = matrix;
                        state.text.line_matrix = matrix;
                    }
                    "Td" => {
                        require_text(in_text, &operator)?;
                        let values = numbers(&operands, 2, &operator)?;
                        state.text.line_matrix =
                            state.text.line_matrix.translated(values[0], values[1]);
                        state.text.matrix = state.text.line_matrix;
                    }
                    "TD" => {
                        require_text(in_text, &operator)?;
                        let values = numbers(&operands, 2, &operator)?;
                        state.text.leading = -values[1];
                        state.text.line_matrix =
                            state.text.line_matrix.translated(values[0], values[1]);
                        state.text.matrix = state.text.line_matrix;
                    }
                    "TL" => {
                        state.text.leading = numbers(&operands, 1, &operator)?[0];
                    }
                    "T*" => {
                        require_text(in_text, &operator)?;
                        require_empty(&operands, &operator)?;
                        state.text.line_matrix =
                            state.text.line_matrix.translated(0.0, -state.text.leading);
                        state.text.matrix = state.text.line_matrix;
                    }
                    "Tc" => {
                        state.text.char_spacing = numbers(&operands, 1, &operator)?[0];
                    }
                    "Tw" => {
                        state.text.word_spacing = numbers(&operands, 1, &operator)?[0];
                    }
                    "Tz" => {
                        let value = numbers(&operands, 1, &operator)?[0];
                        if value <= 0.0 {
                            return Err(malformed("text horizontal scale must be positive"));
                        }
                        state.text.horizontal_scale = value / 100.0;
                    }
                    "Ts" => {
                        state.text.rise = numbers(&operands, 1, &operator)?[0];
                    }
                    "Tr" => {
                        let value = numbers(&operands, 1, &operator)?[0];
                        if value.fract() != 0.0 || !(0.0..=7.0).contains(&value) {
                            return Err(malformed("text rendering mode must be between 0 and 7"));
                        }
                        if value != 0.0 {
                            approximated_graphics = true;
                        }
                    }
                    "Tj" => {
                        require_text(in_text, &operator)?;
                        let bytes = string_operand(&operands, &operator)?;
                        append_text(
                            bytes,
                            &mut state,
                            page_left,
                            page_height,
                            anchor_offset,
                            anchor_length,
                            &mut commands,
                            &mut text_runs,
                        )?;
                    }
                    "TJ" => {
                        require_text(in_text, &operator)?;
                        let array = array_operand(&operands, &operator)?;
                        for item in array {
                            match item {
                                ContentValue::String(bytes) => append_text(
                                    bytes,
                                    &mut state,
                                    page_left,
                                    page_height,
                                    anchor_offset,
                                    anchor_length,
                                    &mut commands,
                                    &mut text_runs,
                                )?,
                                ContentValue::Number(adjustment) => {
                                    let font = state
                                        .text
                                        .font
                                        .as_ref()
                                        .ok_or_else(|| malformed("TJ used before Tf"))?;
                                    state.text.matrix.e -= adjustment / 1000.0 * font.size;
                                }
                                _ => return Err(malformed("TJ array contains an invalid item")),
                            }
                        }
                    }
                    "'" => {
                        require_text(in_text, &operator)?;
                        state.text.line_matrix =
                            state.text.line_matrix.translated(0.0, -state.text.leading);
                        state.text.matrix = state.text.line_matrix;
                        let bytes = string_operand(&operands, &operator)?;
                        append_text(
                            bytes,
                            &mut state,
                            page_left,
                            page_height,
                            anchor_offset,
                            anchor_length,
                            &mut commands,
                            &mut text_runs,
                        )?;
                    }
                    "BMC" => {
                        if operands.len() != 1 {
                            return Err(malformed("BMC requires one tag name"));
                        }
                        name(&operands[0], &operator)?;
                        if marked_content_depth >= MAX_GRAPHICS_DEPTH {
                            return Err(DocsightError::ResourceLimit {
                                resource: "PDF marked-content depth".to_owned(),
                                limit: MAX_GRAPHICS_DEPTH as u64,
                            });
                        }
                        marked_content_depth += 1;
                    }
                    "BDC" => {
                        if operands.len() != 2 {
                            return Err(malformed(
                                "BDC requires a tag name and property-list name",
                            ));
                        }
                        name(&operands[0], &operator)?;
                        if !matches!(
                            operands[1],
                            ContentValue::Name(_) | ContentValue::Dictionary
                        ) {
                            return Err(malformed(
                                "BDC property list must be a name or inline dictionary",
                            ));
                        }
                        if marked_content_depth >= MAX_GRAPHICS_DEPTH {
                            return Err(DocsightError::ResourceLimit {
                                resource: "PDF marked-content depth".to_owned(),
                                limit: MAX_GRAPHICS_DEPTH as u64,
                            });
                        }
                        marked_content_depth += 1;
                    }
                    "EMC" => {
                        require_empty(&operands, &operator)?;
                        marked_content_depth = marked_content_depth
                            .checked_sub(1)
                            .ok_or_else(|| malformed("EMC has no matching BMC or BDC"))?;
                    }
                    "J" | "j" => {
                        let value = numbers(&operands, 1, &operator)?[0];
                        if value.fract() != 0.0 || !(0.0..=2.0).contains(&value) {
                            return Err(malformed(format!(
                                "{operator} line style must be 0, 1, or 2"
                            )));
                        }
                        approximated_graphics = true;
                    }
                    "M" => {
                        let value = numbers(&operands, 1, &operator)?[0];
                        if value < 0.0 {
                            return Err(malformed("miter limit must be non-negative"));
                        }
                        approximated_graphics = true;
                    }
                    "d" => {
                        if operands.len() != 2 {
                            return Err(malformed("d requires a dash array and phase"));
                        }
                        let dash = match &operands[0] {
                            ContentValue::Array(values) => values,
                            _ => return Err(malformed("d requires an array dash pattern")),
                        };
                        if dash.iter().any(|value| {
                            number(value, &operator)
                                .map(|value| value < 0.0)
                                .unwrap_or(true)
                        }) {
                            return Err(malformed("d dash array requires numeric values"));
                        }
                        let phase = number(&operands[1], &operator)?;
                        if phase < 0.0 {
                            return Err(malformed("d dash phase must be non-negative"));
                        }
                        approximated_graphics = true;
                    }
                    "ri" | "i" => {
                        if operator == "ri" {
                            if operands.len() != 1 {
                                return Err(malformed("ri requires one intent name"));
                            }
                            name(&operands[0], &operator)?;
                        } else {
                            let value = numbers(&operands, 1, &operator)?[0];
                            if value < 0.0 {
                                return Err(malformed("flatness must be non-negative"));
                            }
                        }
                        approximated_graphics = true;
                    }
                    "Do" => {
                        if operands.len() != 1 {
                            return Err(malformed("Do requires one XObject name"));
                        }
                        let resource_name = name(&operands[0], &operator)?.to_owned();
                        if !xobjects.contains(&resource_name) {
                            return Err(malformed("Do references an unknown XObject resource"));
                        }
                        let bbox = transformed_unit_bbox(&state.ctm, page_left, page_height)?;
                        commands.push(DisplayCommand::Figure {
                            bbox,
                            resource_name,
                            clips: state.clips.clone(),
                        });
                        approximated_graphics = true;
                        omitted_xobjects = true;
                    }
                    "gs" => {
                        if operands.len() != 1 {
                            return Err(malformed("gs requires one graphics state name"));
                        }
                        name(&operands[0], &operator)?;
                        approximated_graphics = true;
                    }
                    "sh" => {
                        if operands.len() != 1 {
                            return Err(malformed("sh requires one shading name"));
                        }
                        name(&operands[0], &operator)?;
                        approximated_graphics = true;
                    }
                    "W" | "W*" => {
                        require_empty(&operands, &operator)?;
                        if path.is_empty() {
                            return Err(malformed("clipping operator has no current path"));
                        }
                        state.pending_clip = Some(clip_region_from_path(&path, operator == "W*")?);
                    }
                    _ => {
                        return Err(DocsightError::UnsupportedFeature {
                            feature: format!("PDF content operator {operator}"),
                        });
                    }
                }
                if path.len() > MAX_PATH_SEGMENTS {
                    return Err(DocsightError::ResourceLimit {
                        resource: "PDF path segments".to_owned(),
                        limit: MAX_PATH_SEGMENTS as u64,
                    });
                }
                operands.clear();
            }
        }
    }
    if !operands.is_empty() {
        return Err(malformed("PDF content stream ends with unused operands"));
    }
    if in_text {
        return Err(malformed("unterminated PDF text object"));
    }
    if !stack.is_empty() {
        return Err(malformed("unbalanced PDF graphics state"));
    }
    if marked_content_depth != 0 {
        return Err(malformed("unbalanced PDF marked content"));
    }
    if inline_image {
        return Err(malformed("unterminated PDF inline image"));
    }
    Ok(ParsedContent {
        commands,
        text_runs,
        approximated_font,
        approximated_graphics,
        omitted_xobjects,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_text(
    bytes: &[u8],
    state: &mut GraphicsState,
    page_left: f32,
    page_height: f32,
    anchor_offset: u64,
    anchor_length: u64,
    commands: &mut Vec<DisplayCommand>,
    text_runs: &mut Vec<TextRun>,
) -> Result<(), DocsightError> {
    let font = state
        .text
        .font
        .as_ref()
        .ok_or_else(|| malformed("text showing operator used before Tf"))?;
    let text = font.decoder.decode(bytes)?;
    let glyphs = text.chars().count() as f32;
    let spaces = text.chars().filter(|character| *character == ' ').count() as f32;
    let width = glyphs * font.size * 0.6
        + (glyphs - 1.0).max(0.0) * state.text.char_spacing
        + spaces * state.text.word_spacing;
    let width = width * state.text.horizontal_scale;
    let combined = state.text.matrix.concat(state.ctm);
    validate_text_matrix(&combined)?;
    let baseline = combined.transform(0.0, state.text.rise);
    let baseline_y = page_height - baseline.y;
    let text_y0 = state.text.rise - font.size * 0.2;
    let text_y1 = state.text.rise + font.size * 0.8;
    let corners = [
        combined.transform(0.0, text_y0),
        combined.transform(width, text_y0),
        combined.transform(0.0, text_y1),
        combined.transform(width, text_y1),
    ];
    let min_x = corners
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let min_y = corners
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = corners
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let bbox = Rect::new(
        min_x - page_left,
        page_height - max_y,
        max_x - page_left,
        page_height - min_y,
    )
    .map_err(|_| malformed("text operator produced invalid geometry"))?;
    let argb = 0xff00_0000
        | u32::from(state.fill_color.red) << 16
        | u32::from(state.fill_color.green) << 8
        | u32::from(state.fill_color.blue);
    let run = TextRun {
        text,
        bbox,
        baseline_y,
        font_size: font.size,
        font_name: font.font_name.clone(),
        bold: font.bold,
        argb,
        source_offset: anchor_offset,
        source_length: anchor_length,
        clips: state.clips.clone(),
    };
    commands.push(DisplayCommand::Text(run.clone()));
    text_runs.push(run);
    state.text.matrix = state.text.matrix.translated(width, 0.0);
    Ok(())
}

fn append_rectangle(
    path: &mut Vec<PathSegment>,
    matrix: &Matrix,
    page_left: f32,
    page_height: f32,
    values: &[f32],
) -> Result<(), DocsightError> {
    let width = values[2];
    let height = values[3];
    if !width.is_finite() || !height.is_finite() || width == 0.0 || height == 0.0 {
        return Err(malformed(
            "rectangle dimensions must be finite and non-zero",
        ));
    }
    let points = [
        matrix.transform(values[0], values[1]),
        matrix.transform(values[0] + width, values[1]),
        matrix.transform(values[0] + width, values[1] + height),
        matrix.transform(values[0], values[1] + height),
    ];
    path.push(PathSegment::Move(public_point(
        points[0],
        page_left,
        page_height,
    )?));
    path.push(PathSegment::Line(public_point(
        points[1],
        page_left,
        page_height,
    )?));
    path.push(PathSegment::Line(public_point(
        points[2],
        page_left,
        page_height,
    )?));
    path.push(PathSegment::Line(public_point(
        points[3],
        page_left,
        page_height,
    )?));
    path.push(PathSegment::Close);
    Ok(())
}

fn public_point(point: Point, page_left: f32, page_height: f32) -> Result<Point, DocsightError> {
    let point = Point {
        x: point.x - page_left,
        y: page_height - point.y,
    };
    if !point.x.is_finite() || !point.y.is_finite() {
        return Err(malformed("graphics operator produced non-finite geometry"));
    }
    Ok(point)
}

fn transformed_unit_bbox(
    matrix: &Matrix,
    page_left: f32,
    page_height: f32,
) -> Result<Rect, DocsightError> {
    let points = [
        public_point(matrix.transform(0.0, 0.0), page_left, page_height)?,
        public_point(matrix.transform(1.0, 0.0), page_left, page_height)?,
        public_point(matrix.transform(0.0, 1.0), page_left, page_height)?,
        public_point(matrix.transform(1.0, 1.0), page_left, page_height)?,
    ];
    let x0 = points
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let y0 = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let x1 = points
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let y1 = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    Rect::new(x0, y0, x1, y1).map_err(|_| malformed("XObject transform produced invalid geometry"))
}

fn validate_text_matrix(matrix: &Matrix) -> Result<(), DocsightError> {
    let values = [matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f];
    if values.iter().any(|value| !value.is_finite()) {
        return Err(malformed("text matrix contains non-finite values"));
    }
    if (matrix.a * matrix.d - matrix.b * matrix.c).abs() <= f32::EPSILON {
        return Err(DocsightError::UnsupportedFeature {
            feature: "degenerate PDF text matrix".to_owned(),
        });
    }
    Ok(())
}

fn rgb(red: f32, green: f32, blue: f32) -> Result<Color, DocsightError> {
    let values = [red, green, blue];
    if values
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(malformed("color components must be between zero and one"));
    }
    Ok(Color {
        red: (red * 255.0).round() as u8,
        green: (green * 255.0).round() as u8,
        blue: (blue * 255.0).round() as u8,
    })
}

fn cmyk(cyan: f32, magenta: f32, yellow: f32, black: f32) -> Result<Color, DocsightError> {
    let values = [cyan, magenta, yellow, black];
    if values
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(malformed("CMYK components must be between zero and one"));
    }
    Ok(Color {
        red: ((1.0 - cyan) * (1.0 - black) * 255.0).round() as u8,
        green: ((1.0 - magenta) * (1.0 - black) * 255.0).round() as u8,
        blue: ((1.0 - yellow) * (1.0 - black) * 255.0).round() as u8,
    })
}

fn color_components(values: &[f32], operator: &str) -> Result<Color, DocsightError> {
    match values {
        [gray] => rgb(*gray, *gray, *gray),
        [red, green, blue] => rgb(*red, *green, *blue),
        [cyan, magenta, yellow, black] => cmyk(*cyan, *magenta, *yellow, *black),
        _ => Err(malformed(format!(
            "{operator} has an unsupported component count"
        ))),
    }
}

fn require_text(in_text: bool, operator: &str) -> Result<(), DocsightError> {
    if in_text {
        Ok(())
    } else {
        Err(malformed(format!("{operator} used outside a text object")))
    }
}

fn require_empty(operands: &[ContentValue], operator: &str) -> Result<(), DocsightError> {
    if operands.is_empty() {
        Ok(())
    } else {
        Err(malformed(format!("{operator} does not accept operands")))
    }
}

fn require_open_subpath(path: &[PathSegment], operator: &str) -> Result<(), DocsightError> {
    let open = path.iter().rev().find_map(|segment| match segment {
        PathSegment::Move(_) => Some(true),
        PathSegment::Close => Some(false),
        _ => None,
    });
    if open == Some(true) {
        Ok(())
    } else {
        Err(malformed(format!(
            "{operator} requires an open current subpath"
        )))
    }
}

fn current_path_point(path: &[PathSegment]) -> Result<Point, DocsightError> {
    path.iter()
        .rev()
        .find_map(|segment| match segment {
            PathSegment::Move(point) | PathSegment::Line(point) => Some(*point),
            PathSegment::Cubic(_, _, point) => Some(*point),
            PathSegment::Close => None,
        })
        .ok_or_else(|| malformed("curve operator has no current point"))
}

fn numbers(
    operands: &[ContentValue],
    expected: usize,
    operator: &str,
) -> Result<Vec<f32>, DocsightError> {
    if operands.len() != expected {
        return Err(malformed(format!(
            "{operator} requires {expected} numeric operands"
        )));
    }
    operands
        .iter()
        .map(|value| number(value, operator))
        .collect()
}

fn number(value: &ContentValue, operator: &str) -> Result<f32, DocsightError> {
    match value {
        ContentValue::Number(value) if value.is_finite() => Ok(*value),
        _ => Err(malformed(format!("{operator} requires numeric operands"))),
    }
}

fn name<'a>(value: &'a ContentValue, operator: &str) -> Result<&'a str, DocsightError> {
    match value {
        ContentValue::Name(value) => Ok(value),
        _ => Err(malformed(format!("{operator} requires a name operand"))),
    }
}

fn string_operand<'a>(
    operands: &'a [ContentValue],
    operator: &str,
) -> Result<&'a [u8], DocsightError> {
    if operands.len() != 1 {
        return Err(malformed(format!("{operator} requires one string operand")));
    }
    match &operands[0] {
        ContentValue::String(value) => Ok(value),
        _ => Err(malformed(format!("{operator} requires a string operand"))),
    }
}

fn array_operand<'a>(
    operands: &'a [ContentValue],
    operator: &str,
) -> Result<&'a [ContentValue], DocsightError> {
    if operands.len() != 1 {
        return Err(malformed(format!("{operator} requires one array operand")));
    }
    match &operands[0] {
        ContentValue::Array(value) => Ok(value),
        _ => Err(malformed(format!("{operator} requires an array operand"))),
    }
}

fn validate_inline_image_dictionary(operands: &[ContentValue]) -> Result<(), DocsightError> {
    if operands.is_empty() || operands.len() % 2 != 0 {
        return Err(malformed(
            "inline PDF image dictionary requires key-value pairs",
        ));
    }
    for pair in operands.chunks_exact(2) {
        if !matches!(pair[0], ContentValue::Name(_)) {
            return Err(malformed("inline PDF image dictionary keys must be names"));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    fill_color: Color,
    stroke_color: Color,
    line_width: f32,
    text: TextState,
    clips: Vec<ClipRegion>,
    pending_clip: Option<ClipRegion>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::identity(),
            fill_color: Color {
                red: 0,
                green: 0,
                blue: 0,
            },
            stroke_color: Color {
                red: 0,
                green: 0,
                blue: 0,
            },
            line_width: 1.0,
            text: TextState::default(),
            clips: Vec::new(),
            pending_clip: None,
        }
    }
}

#[derive(Clone)]
struct TextState {
    matrix: Matrix,
    line_matrix: Matrix,
    font: Option<FontSelection>,
    leading: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    rise: f32,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            matrix: Matrix::identity(),
            line_matrix: Matrix::identity(),
            font: None,
            leading: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            rise: 0.0,
        }
    }
}

#[derive(Clone)]
struct FontSelection {
    size: f32,
    bold: bool,
    font_name: String,
    decoder: FontDecoder,
}

#[derive(Clone, Copy)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

fn commit_pending_clip(state: &mut GraphicsState) -> Result<(), DocsightError> {
    if let Some(clip) = state.pending_clip.take() {
        if state.clips.len() >= MAX_GRAPHICS_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF clipping path depth".to_owned(),
                limit: MAX_GRAPHICS_DEPTH as u64,
            });
        }
        state.clips.push(clip);
    }
    Ok(())
}

fn clip_region_from_path(
    path: &[PathSegment],
    even_odd: bool,
) -> Result<ClipRegion, DocsightError> {
    let polygons = flatten_clip_path(path)?;
    if polygons.is_empty() || polygons.iter().any(|polygon| polygon.len() < 3) {
        return Err(malformed("clipping path has no closed area"));
    }
    Ok(ClipRegion { polygons, even_odd })
}

fn flatten_clip_path(path: &[PathSegment]) -> Result<Vec<Vec<Point>>, DocsightError> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut cursor = None;
    for segment in path {
        match segment {
            PathSegment::Move(point) => {
                if !current.is_empty() {
                    close_polygon(&mut current);
                    result.push(std::mem::take(&mut current));
                }
                current.push(*point);
                cursor = Some(*point);
            }
            PathSegment::Line(point) => {
                if cursor.is_none() {
                    return Err(malformed("clipping line has no starting point"));
                }
                current.push(*point);
                cursor = Some(*point);
            }
            PathSegment::Cubic(first, second, end) => {
                let start =
                    cursor.ok_or_else(|| malformed("clipping curve has no starting point"))?;
                for step in 1..=24 {
                    let t = step as f32 / 24.0;
                    let inverse = 1.0 - t;
                    current.push(Point {
                        x: inverse.powi(3) * start.x
                            + 3.0 * inverse.powi(2) * t * first.x
                            + 3.0 * inverse * t.powi(2) * second.x
                            + t.powi(3) * end.x,
                        y: inverse.powi(3) * start.y
                            + 3.0 * inverse.powi(2) * t * first.y
                            + 3.0 * inverse * t.powi(2) * second.y
                            + t.powi(3) * end.y,
                    });
                }
                cursor = Some(*end);
            }
            PathSegment::Close => {
                close_polygon(&mut current);
            }
        }
    }
    if !current.is_empty() {
        close_polygon(&mut current);
        result.push(current);
    }
    Ok(result)
}

fn close_polygon(polygon: &mut Vec<Point>) {
    if let Some(first) = polygon.first().copied()
        && polygon.last().copied() != Some(first)
    {
        polygon.push(first);
    }
}

impl Matrix {
    fn identity() -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }

    fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    fn transform(self, x: f32, y: f32) -> Point {
        Point {
            x: x * self.a + y * self.c + self.e,
            y: x * self.b + y * self.d + self.f,
        }
    }

    fn concat(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    fn translated(self, x: f32, y: f32) -> Self {
        Self {
            e: self.e + x * self.a + y * self.c,
            f: self.f + x * self.b + y * self.d,
            ..self
        }
    }

    fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|value| value.is_finite())
    }

    fn stroke_width(self, width: f32) -> Result<f32, DocsightError> {
        let horizontal = (self.a * self.a + self.b * self.b).sqrt();
        let vertical = (self.c * self.c + self.d * self.d).sqrt();
        if !horizontal.is_finite() || !vertical.is_finite() {
            return Err(malformed("stroke transform produced a non-finite width"));
        }
        if (horizontal - vertical).abs() > 0.0001 {
            return Err(DocsightError::UnsupportedFeature {
                feature: "non-uniformly transformed PDF strokes".to_owned(),
            });
        }
        let transformed = width * horizontal;
        if !transformed.is_finite() || transformed < 0.0 {
            return Err(malformed("stroke transform produced an invalid width"));
        }
        Ok(transformed)
    }
}

#[derive(Clone, Debug)]
enum ContentValue {
    Number(f32),
    Name(String),
    String(Vec<u8>),
    Array(Vec<ContentValue>),
    Dictionary,
}

enum ContentToken {
    Operand(ContentValue),
    Operator(String, usize, usize),
}

struct ContentLexer<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ContentLexer<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn next_token(&mut self) -> Result<Option<ContentToken>, DocsightError> {
        self.skip_space_and_comments();
        let byte = match self.peek() {
            Some(byte) => byte,
            None => return Ok(None),
        };
        let token = match byte {
            b'/' => ContentToken::Operand(ContentValue::Name(self.parse_name()?)),
            b'(' => ContentToken::Operand(ContentValue::String(self.parse_string()?)),
            b'<' if self.bytes.get(self.cursor + 1) != Some(&b'<') => {
                ContentToken::Operand(ContentValue::String(self.parse_hex_string()?))
            }
            b'<' => ContentToken::Operand(self.parse_dictionary()?),
            b'[' => ContentToken::Operand(ContentValue::Array(self.parse_array()?)),
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                ContentToken::Operand(ContentValue::Number(self.parse_number()?))
            }
            _ => {
                let start = self.cursor;
                let operator = self.parse_operator()?;
                let end = self.cursor;
                ContentToken::Operator(operator, start, end)
            }
        };
        Ok(Some(token))
    }

    fn parse_array(&mut self) -> Result<Vec<ContentValue>, DocsightError> {
        self.cursor += 1;
        let mut values = Vec::new();
        loop {
            self.skip_space_and_comments();
            match self.peek() {
                Some(b']') => {
                    self.cursor += 1;
                    return Ok(values);
                }
                Some(b'(') => values.push(ContentValue::String(self.parse_string()?)),
                Some(b'<') if self.bytes.get(self.cursor + 1) != Some(&b'<') => {
                    values.push(ContentValue::String(self.parse_hex_string()?));
                }
                Some(b'<') => values.push(self.parse_dictionary()?),
                Some(b'/') => values.push(ContentValue::Name(self.parse_name()?)),
                Some(b'+' | b'-' | b'.' | b'0'..=b'9') => {
                    values.push(ContentValue::Number(self.parse_number()?));
                }
                Some(_) => return Err(malformed("invalid PDF content array item")),
                None => return Err(malformed("unterminated PDF content array")),
            }
        }
    }

    fn parse_dictionary(&mut self) -> Result<ContentValue, DocsightError> {
        if self.bytes.get(self.cursor..self.cursor + 2) != Some(b"<<") {
            return Err(malformed("invalid inline PDF dictionary"));
        }
        self.cursor += 2;
        let mut depth = 1usize;
        while self.cursor < self.bytes.len() {
            if self.bytes.get(self.cursor..self.cursor + 2) == Some(b"<<") {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| malformed("PDF dictionary depth overflow"))?;
                if depth > MAX_STRING_DEPTH {
                    return Err(DocsightError::ResourceLimit {
                        resource: "PDF inline dictionary depth".to_owned(),
                        limit: MAX_STRING_DEPTH as u64,
                    });
                }
                self.cursor += 2;
            } else if self.bytes.get(self.cursor..self.cursor + 2) == Some(b">>") {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| malformed("unbalanced inline PDF dictionary"))?;
                self.cursor += 2;
                if depth == 0 {
                    return Ok(ContentValue::Dictionary);
                }
            } else {
                self.cursor += 1;
            }
        }
        Err(malformed("unterminated inline PDF dictionary"))
    }

    fn parse_string(&mut self) -> Result<Vec<u8>, DocsightError> {
        self.cursor += 1;
        let mut result = Vec::new();
        let mut depth = 1usize;
        while let Some(byte) = self.take() {
            match byte {
                b'(' => {
                    if depth >= MAX_STRING_DEPTH {
                        return Err(DocsightError::ResourceLimit {
                            resource: "PDF content string nesting".to_owned(),
                            limit: MAX_STRING_DEPTH as u64,
                        });
                    }
                    depth += 1;
                    result.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(result);
                    }
                    result.push(byte);
                }
                b'\\' => parse_content_escape(self, &mut result)?,
                _ => result.push(byte),
            }
        }
        Err(malformed("unterminated PDF content string"))
    }

    fn parse_hex_string(&mut self) -> Result<Vec<u8>, DocsightError> {
        self.cursor += 1;
        let mut nibbles = Vec::new();
        loop {
            let byte = self
                .take()
                .ok_or_else(|| malformed("unterminated PDF content hex string"))?;
            match byte {
                b'>' => break,
                byte if is_space(byte) => {}
                _ => nibbles.push(hex_digit(byte)?),
            }
        }
        let mut result = Vec::with_capacity(nibbles.len().div_ceil(2));
        let mut pairs = nibbles.chunks_exact(2);
        for pair in &mut pairs {
            result.push(pair[0] << 4 | pair[1]);
        }
        if let [high] = pairs.remainder() {
            result.push(high << 4);
        }
        Ok(result)
    }

    fn parse_name(&mut self) -> Result<String, DocsightError> {
        self.cursor += 1;
        let mut bytes = Vec::new();
        while self.peek().is_some_and(|byte| !is_delimiter(byte)) {
            let byte = self.take().ok_or_else(|| malformed("truncated PDF name"))?;
            if byte == b'#' {
                let high = self
                    .take()
                    .ok_or_else(|| malformed("truncated PDF name escape"))?;
                let low = self
                    .take()
                    .ok_or_else(|| malformed("truncated PDF name escape"))?;
                bytes.push(hex_digit(high)? << 4 | hex_digit(low)?);
            } else {
                bytes.push(byte);
            }
        }
        if bytes.is_empty() {
            return Err(malformed("empty PDF content name"));
        }
        String::from_utf8(bytes).map_err(|_| malformed("PDF content name is not UTF-8"))
    }

    fn parse_number(&mut self) -> Result<f32, DocsightError> {
        let start = self.cursor;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.'))
        {
            self.cursor += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.cursor])
            .map_err(|_| malformed("PDF content number is not ASCII"))?;
        let value = text
            .parse::<f32>()
            .map_err(|_| malformed("invalid PDF content number"))?;
        if !value.is_finite() {
            return Err(malformed("non-finite PDF content number"));
        }
        Ok(value)
    }

    fn parse_operator(&mut self) -> Result<String, DocsightError> {
        let start = self.cursor;
        while self.peek().is_some_and(|byte| !is_delimiter(byte)) {
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(malformed(format!(
                "invalid PDF content token at byte {start} (0x{:02x})",
                self.bytes[start]
            )));
        }
        String::from_utf8(self.bytes[start..self.cursor].to_vec())
            .map_err(|_| malformed("PDF content operator is not ASCII"))
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.peek().is_some_and(is_space) {
                self.cursor += 1;
            }
            if self.peek() != Some(b'%') {
                break;
            }
            while let Some(byte) = self.take() {
                if byte == b'\r' || byte == b'\n' {
                    break;
                }
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.cursor += 1;
        Some(byte)
    }

    fn skip_inline_image(&mut self) -> Result<(), DocsightError> {
        while self.peek().is_some_and(is_space) {
            self.cursor += 1;
        }
        let data_start = self.cursor;
        let mut cursor = data_start;
        while cursor + 2 < self.bytes.len() {
            if self.bytes[cursor] == b'E'
                && self.bytes[cursor + 1] == b'I'
                && is_space(self.bytes[cursor.saturating_sub(1)])
                && is_space(self.bytes[cursor + 2])
            {
                self.cursor = cursor + 2;
                return Ok(());
            }
            cursor += 1;
        }
        Err(malformed("inline PDF image has no EI terminator"))
    }
}

fn parse_content_escape(
    lexer: &mut ContentLexer<'_>,
    result: &mut Vec<u8>,
) -> Result<(), DocsightError> {
    let byte = lexer
        .take()
        .ok_or_else(|| malformed("truncated PDF content string escape"))?;
    match byte {
        b'n' => result.push(b'\n'),
        b'r' => result.push(b'\r'),
        b't' => result.push(b'\t'),
        b'b' => result.push(8),
        b'f' => result.push(12),
        b'(' | b')' | b'\\' => result.push(byte),
        b'\r' => {
            if lexer.peek() == Some(b'\n') {
                lexer.cursor += 1;
            }
        }
        b'\n' => {}
        b'0'..=b'7' => {
            let mut value = u16::from(byte - b'0');
            for _ in 0..2 {
                match lexer.peek() {
                    Some(next @ b'0'..=b'7') => {
                        lexer.cursor += 1;
                        value = value * 8 + u16::from(next - b'0');
                    }
                    _ => break,
                }
            }
            result.push(u8::try_from(value).map_err(|_| malformed("invalid octal escape"))?);
        }
        _ => result.push(byte),
    }
    Ok(())
}

fn is_space(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter(byte: u8) -> bool {
    is_space(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

pub(crate) fn fonts_from_resources(
    resources: &BTreeMap<String, Value>,
    resolve: impl Fn(&Value) -> Result<Value, DocsightError>,
    decode_stream_value: impl Fn(&Value) -> Result<Vec<u8>, DocsightError>,
) -> Result<BTreeMap<String, FontInfo>, DocsightError> {
    let font_value = match resources.get("Font") {
        Some(value) => resolve(value)?,
        None => return Ok(BTreeMap::new()),
    };
    let font_dict = match font_value {
        Value::Dict(dict) => dict,
        _ => return Err(malformed("Font resource must be a dictionary")),
    };
    let mut fonts = BTreeMap::new();
    for (resource_name, value) in font_dict {
        let value = resolve(&value)?;
        let dict = match value {
            Value::Dict(dict) => dict,
            _ => return Err(malformed("font resource must resolve to a dictionary")),
        };
        let subtype = dict
            .get("Subtype")
            .ok_or_else(|| malformed("font has no Subtype"))?;
        let subtype = match subtype {
            Value::Name(value) => value.as_str(),
            _ => return Err(malformed("font Subtype must be a name")),
        };
        if !matches!(subtype, "Type0" | "Type1" | "TrueType") {
            return Err(DocsightError::UnsupportedFeature {
                feature: "PDF fonts other than Type0, Type1, or TrueType".to_owned(),
            });
        }
        let descendant = if subtype == "Type0" {
            let descendants = match dict.get("DescendantFonts") {
                Some(value) => resolve(value)?,
                None => return Err(malformed("Type0 font has no DescendantFonts")),
            };
            let descendants = match descendants {
                Value::Array(values) if !values.is_empty() => values,
                _ => return Err(malformed("Type0 DescendantFonts must be a non-empty array")),
            };
            let descendant = resolve(&descendants[0])?;
            let descendant = match descendant {
                Value::Dict(dict) => dict,
                _ => {
                    return Err(malformed(
                        "Type0 descendant font must resolve to a dictionary",
                    ));
                }
            };
            let descendant_subtype = match descendant.get("Subtype") {
                Some(Value::Name(value)) => value.as_str(),
                _ => return Err(malformed("Type0 descendant has no valid Subtype")),
            };
            if !matches!(descendant_subtype, "CIDFontType0" | "CIDFontType2") {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF Type0 descendant font {descendant_subtype}"),
                });
            }
            Some(descendant)
        } else {
            None
        };
        let base_font = dict
            .get("BaseFont")
            .or_else(|| descendant.as_ref().and_then(|dict| dict.get("BaseFont")))
            .ok_or_else(|| malformed("font has no BaseFont"))?;
        let base_font = match base_font {
            Value::Name(value) => value.as_str(),
            _ => return Err(malformed("BaseFont must be a name")),
        };
        let canonical_font = canonical_base_font(base_font);
        let bold = match canonical_font {
            "Helvetica" | "Helvetica-Oblique" | "Times-Roman" | "Times-Italic" | "Courier"
            | "Courier-Oblique" | "Symbol" | "ZapfDingbats" => false,
            "Helvetica-Bold"
            | "Helvetica-BoldOblique"
            | "Times-Bold"
            | "Times-BoldItalic"
            | "Courier-Bold"
            | "Courier-BoldOblique" => true,
            _ if matches!(subtype, "Type0" | "Type1" | "TrueType") => {
                canonical_font.contains("Bold")
            }
            _ => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF base font {base_font}"),
                });
            }
        };
        let decoder = match dict.get("ToUnicode").or_else(|| {
            descendant
                .as_ref()
                .and_then(|descendant| descendant.get("ToUnicode"))
        }) {
            Some(value) => FontDecoder::ToUnicode(parse_to_unicode(&decode_stream_value(value)?)?),
            None if subtype == "Type0" => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: "Type0 PDF font without ToUnicode CMap".to_owned(),
                });
            }
            None if subtype == "TrueType" && !dict.contains_key("Encoding") => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: "TrueType PDF font without ToUnicode or an explicit encoding"
                        .to_owned(),
                });
            }
            None => decoder_from_encoding(dict.get("Encoding"))?,
        };
        fonts.insert(
            resource_name,
            FontInfo {
                bold,
                base_font: canonical_font.to_owned(),
                decoder,
            },
        );
    }
    Ok(fonts)
}

impl FontDecoder {
    fn decode(&self, bytes: &[u8]) -> Result<String, DocsightError> {
        match self {
            Self::Ascii => decode_ascii(bytes),
            Self::WinAnsi => decode_win_ansi(bytes),
            Self::ToUnicode(map) => map.decode(bytes),
        }
    }
}

impl ToUnicodeMap {
    fn decode(&self, bytes: &[u8]) -> Result<String, DocsightError> {
        let mut cursor = 0usize;
        let mut output = String::new();
        while cursor < bytes.len() {
            let mapping = self.code_lengths.iter().find_map(|length| {
                let end = cursor.checked_add(*length)?;
                let code = bytes.get(cursor..end)?;
                self.mappings.get(code).map(|value| (*length, value))
            });
            let Some((length, value)) = mapping else {
                return Err(DocsightError::UnsupportedFeature {
                    feature: "PDF text code missing from ToUnicode CMap".to_owned(),
                });
            };
            output.push_str(value);
            cursor += length;
        }
        Ok(output)
    }
}

fn decoder_from_encoding(encoding: Option<&Value>) -> Result<FontDecoder, DocsightError> {
    match encoding {
        None => Ok(FontDecoder::Ascii),
        Some(Value::Name(name)) if name == "WinAnsiEncoding" => Ok(FontDecoder::WinAnsi),
        Some(Value::Name(name)) => Err(DocsightError::UnsupportedFeature {
            feature: format!("PDF font encoding {name}"),
        }),
        Some(Value::Dict(_)) => Err(DocsightError::UnsupportedFeature {
            feature: "PDF font encoding differences".to_owned(),
        }),
        Some(_) => Err(malformed("font Encoding must be a name or dictionary")),
    }
}

fn decode_ascii(bytes: &[u8]) -> Result<String, DocsightError> {
    if bytes.iter().any(|byte| !(32..=126).contains(byte)) {
        return Err(DocsightError::UnsupportedFeature {
            feature: "non-ASCII PDF text without an explicit supported encoding".to_owned(),
        });
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| malformed("PDF text is not valid ASCII"))
}

fn decode_win_ansi(bytes: &[u8]) -> Result<String, DocsightError> {
    bytes.iter().map(|byte| win_ansi_character(*byte)).collect()
}

fn win_ansi_character(byte: u8) -> Result<char, DocsightError> {
    let character = match byte {
        32..=126 | 160..=255 => char::from_u32(u32::from(byte)),
        128 => Some('\u{20ac}'),
        130 => Some('\u{201a}'),
        131 => Some('\u{0192}'),
        132 => Some('\u{201e}'),
        133 => Some('\u{2026}'),
        134 => Some('\u{2020}'),
        135 => Some('\u{2021}'),
        136 => Some('\u{02c6}'),
        137 => Some('\u{2030}'),
        138 => Some('\u{0160}'),
        139 => Some('\u{2039}'),
        140 => Some('\u{0152}'),
        142 => Some('\u{017d}'),
        145 => Some('\u{2018}'),
        146 => Some('\u{2019}'),
        147 => Some('\u{201c}'),
        148 => Some('\u{201d}'),
        149 => Some('\u{2022}'),
        150 => Some('\u{2013}'),
        151 => Some('\u{2014}'),
        152 => Some('\u{02dc}'),
        153 => Some('\u{2122}'),
        154 => Some('\u{0161}'),
        155 => Some('\u{203a}'),
        156 => Some('\u{0153}'),
        158 => Some('\u{017e}'),
        159 => Some('\u{0178}'),
        _ => None,
    };
    character.ok_or_else(|| DocsightError::UnsupportedFeature {
        feature: format!("undefined WinAnsi character code {byte}"),
    })
}

const MAX_CMAP_MAPPINGS: usize = 65_536;
const MAX_CMAP_TOKENS: usize = 262_144;

#[derive(Clone, Debug)]
enum CMapToken {
    Integer(usize),
    Hex(Vec<u8>),
    Keyword(String),
    ArrayStart,
    ArrayEnd,
}

fn parse_to_unicode(bytes: &[u8]) -> Result<ToUnicodeMap, DocsightError> {
    let tokens = tokenize_cmap(bytes)?;
    let mut mappings = BTreeMap::new();
    let mut cursor = 0usize;
    while cursor < tokens.len() {
        match tokens.get(cursor) {
            Some(CMapToken::Keyword(keyword)) if keyword == "beginbfchar" => {
                let count = preceding_count(&tokens, cursor)?;
                cursor += 1;
                for _ in 0..count {
                    let source = cmap_hex(&tokens, cursor)?.to_vec();
                    let target = decode_utf16be(cmap_hex(&tokens, cursor + 1)?)?;
                    insert_cmap_mapping(&mut mappings, source, target)?;
                    cursor += 2;
                }
            }
            Some(CMapToken::Keyword(keyword)) if keyword == "beginbfrange" => {
                let count = preceding_count(&tokens, cursor)?;
                cursor += 1;
                for _ in 0..count {
                    let start = cmap_hex(&tokens, cursor)?.to_vec();
                    let end = cmap_hex(&tokens, cursor + 1)?.to_vec();
                    cursor += 2;
                    let sources = cmap_code_range(&start, &end)?;
                    match tokens.get(cursor) {
                        Some(CMapToken::Hex(target_start)) => {
                            let mut target = target_start.clone();
                            let source_count = sources.len();
                            for (index, source) in sources.into_iter().enumerate() {
                                insert_cmap_mapping(
                                    &mut mappings,
                                    source,
                                    decode_utf16be(&target)?,
                                )?;
                                if index + 1 < source_count {
                                    increment_big_endian(&mut target)?;
                                }
                            }
                            cursor += 1;
                        }
                        Some(CMapToken::ArrayStart) => {
                            cursor += 1;
                            for source in sources {
                                let target = decode_utf16be(cmap_hex(&tokens, cursor)?)?;
                                insert_cmap_mapping(&mut mappings, source, target)?;
                                cursor += 1;
                            }
                            if !matches!(tokens.get(cursor), Some(CMapToken::ArrayEnd)) {
                                return Err(malformed("ToUnicode bfrange array has wrong length"));
                            }
                            cursor += 1;
                        }
                        _ => return Err(malformed("invalid ToUnicode bfrange target")),
                    }
                }
            }
            _ => cursor += 1,
        }
    }
    if mappings.is_empty() {
        return Err(malformed("ToUnicode CMap has no supported mappings"));
    }
    let mut code_lengths = mappings.keys().map(Vec::len).collect::<Vec<_>>();
    code_lengths.sort_unstable();
    code_lengths.dedup();
    code_lengths.reverse();
    Ok(ToUnicodeMap {
        mappings,
        code_lengths,
    })
}

fn tokenize_cmap(bytes: &[u8]) -> Result<Vec<CMapToken>, DocsightError> {
    let mut tokens = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        match bytes[cursor] {
            byte if is_space(byte) => cursor += 1,
            b'%' => {
                while cursor < bytes.len() && !matches!(bytes[cursor], b'\r' | b'\n') {
                    cursor += 1;
                }
            }
            b'[' => {
                tokens.push(CMapToken::ArrayStart);
                cursor += 1;
            }
            b']' => {
                tokens.push(CMapToken::ArrayEnd);
                cursor += 1;
            }
            b'<' if bytes.get(cursor + 1) != Some(&b'<') => {
                cursor += 1;
                let mut digits = Vec::new();
                while cursor < bytes.len() && bytes[cursor] != b'>' {
                    if !is_space(bytes[cursor]) {
                        digits.push(bytes[cursor]);
                    }
                    cursor += 1;
                }
                if cursor == bytes.len() {
                    return Err(malformed("unterminated ToUnicode hex string"));
                }
                cursor += 1;
                tokens.push(CMapToken::Hex(decode_hex_digits(&digits)?));
            }
            b'<' | b'>' => {
                cursor += if bytes.get(cursor + 1) == Some(&bytes[cursor]) {
                    2
                } else {
                    1
                }
            }
            _ => {
                let start = cursor;
                while cursor < bytes.len()
                    && !is_space(bytes[cursor])
                    && !matches!(bytes[cursor], b'[' | b']' | b'<' | b'>' | b'%')
                {
                    cursor += 1;
                }
                let word = std::str::from_utf8(&bytes[start..cursor])
                    .map_err(|_| malformed("ToUnicode token is not ASCII"))?;
                match word.parse::<usize>() {
                    Ok(value) => tokens.push(CMapToken::Integer(value)),
                    Err(_) => tokens.push(CMapToken::Keyword(word.to_owned())),
                }
            }
        }
        if tokens.len() > MAX_CMAP_TOKENS {
            return Err(DocsightError::ResourceLimit {
                resource: "ToUnicode CMap tokens".to_owned(),
                limit: MAX_CMAP_TOKENS as u64,
            });
        }
    }
    Ok(tokens)
}

fn decode_hex_digits(digits: &[u8]) -> Result<Vec<u8>, DocsightError> {
    if digits.is_empty() || digits.len() % 2 != 0 {
        return Err(malformed(
            "ToUnicode hex string must contain complete bytes",
        ));
    }
    digits
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok(high << 4 | low)
        })
        .collect()
}

fn hex_digit(byte: u8) -> Result<u8, DocsightError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(malformed("ToUnicode hex string contains a non-hex digit")),
    }
}

fn preceding_count(tokens: &[CMapToken], cursor: usize) -> Result<usize, DocsightError> {
    match cursor.checked_sub(1).and_then(|index| tokens.get(index)) {
        Some(CMapToken::Integer(count)) if *count <= MAX_CMAP_MAPPINGS => Ok(*count),
        Some(CMapToken::Integer(_)) => Err(DocsightError::ResourceLimit {
            resource: "ToUnicode CMap mappings".to_owned(),
            limit: MAX_CMAP_MAPPINGS as u64,
        }),
        _ => Err(malformed("ToUnicode mapping block has no valid count")),
    }
}

fn cmap_hex(tokens: &[CMapToken], cursor: usize) -> Result<&[u8], DocsightError> {
    match tokens.get(cursor) {
        Some(CMapToken::Hex(value)) => Ok(value),
        _ => Err(malformed("ToUnicode mapping requires a hex string")),
    }
}

fn decode_utf16be(bytes: &[u8]) -> Result<String, DocsightError> {
    if bytes.is_empty() || bytes.len() % 2 != 0 {
        return Err(malformed("ToUnicode target must be UTF-16BE"));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
    std::char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| malformed("ToUnicode target is invalid UTF-16BE"))
}

fn cmap_code_range(start: &[u8], end: &[u8]) -> Result<Vec<Vec<u8>>, DocsightError> {
    if start.is_empty() || start.len() != end.len() || start.len() > 4 {
        return Err(malformed("ToUnicode source range has invalid code widths"));
    }
    let start_value = big_endian_value(start);
    let end_value = big_endian_value(end);
    if end_value < start_value {
        return Err(malformed("ToUnicode source range is descending"));
    }
    let count = end_value - start_value + 1;
    if count > MAX_CMAP_MAPPINGS as u64 {
        return Err(DocsightError::ResourceLimit {
            resource: "ToUnicode CMap mappings".to_owned(),
            limit: MAX_CMAP_MAPPINGS as u64,
        });
    }
    (start_value..=end_value)
        .map(|value| big_endian_bytes(value, start.len()))
        .collect()
}

fn big_endian_value(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |value, byte| value << 8 | u64::from(*byte))
}

fn big_endian_bytes(value: u64, length: usize) -> Result<Vec<u8>, DocsightError> {
    let bytes = value.to_be_bytes();
    let start = bytes
        .len()
        .checked_sub(length)
        .ok_or_else(|| malformed("invalid CMap code width"))?;
    Ok(bytes[start..].to_vec())
}

fn increment_big_endian(bytes: &mut [u8]) -> Result<(), DocsightError> {
    for byte in bytes.iter_mut().rev() {
        let (value, overflow) = byte.overflowing_add(1);
        *byte = value;
        if !overflow {
            return Ok(());
        }
    }
    Err(malformed("ToUnicode destination range overflows"))
}

fn insert_cmap_mapping(
    mappings: &mut BTreeMap<Vec<u8>, String>,
    source: Vec<u8>,
    target: String,
) -> Result<(), DocsightError> {
    if source.is_empty() || source.len() > 4 {
        return Err(malformed("ToUnicode source code has invalid width"));
    }
    if mappings.len() >= MAX_CMAP_MAPPINGS && !mappings.contains_key(&source) {
        return Err(DocsightError::ResourceLimit {
            resource: "ToUnicode CMap mappings".to_owned(),
            limit: MAX_CMAP_MAPPINGS as u64,
        });
    }
    if mappings.insert(source, target).is_some() {
        return Err(malformed("ToUnicode CMap contains duplicate source codes"));
    }
    Ok(())
}

fn canonical_base_font(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, base))
            if tag.len() == 6 && tag.bytes().all(|byte| byte.is_ascii_uppercase()) =>
        {
            base
        }
        _ => name,
    }
}
