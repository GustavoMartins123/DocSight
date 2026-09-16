use crate::font::{FontProgram, GlyphOutline};
use crate::syntax::{Value, malformed};
use docsight_core::{DocsightError, ErrorLocation, Rect};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const MAX_OPERATIONS: usize = 1_000_000;
const MAX_CID_TO_GID_ENTRIES: usize = 65_536;
const MAX_GRAPHICS_DEPTH: usize = 64;
const MAX_PATH_SEGMENTS: usize = 100_000;
const MAX_OPERANDS: usize = 100_000;
const MAX_STRING_DEPTH: usize = 64;
const MAX_DASH_ENTRIES: usize = 1_024;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Paint {
    pub color: Color,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StrokeStyle {
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dash: Vec<f32>,
    pub dash_phase: f32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExtGraphicsState {
    pub fill_alpha: Option<f32>,
    pub stroke_alpha: Option<f32>,
    pub line_width: Option<f32>,
    pub line_cap: Option<LineCap>,
    pub line_join: Option<LineJoin>,
    pub miter_limit: Option<f32>,
    pub dash: Option<(Vec<f32>, f32)>,
    pub font: Option<(FontInfo, f32)>,
    pub ignored_keys: BTreeSet<String>,
    pub blend_modes: Option<Vec<String>>,
    pub soft_mask: Option<bool>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum VisualIssue {
    ExtGraphicsState(String),
    SoftMask,
    BlendMode(String),
    ColorSpace(String),
    Pattern(String),
    TextClip,
    NegativeFontSize,
    NonUniformStroke,
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
    pub stroke_argb: u32,
    pub stroke_style: StrokeStyle,
    pub render_mode: u8,
    pub mirrored_x: bool,
    pub source_offset: u64,
    pub source_length: u64,
    pub clips: Vec<ClipRegion>,
    pub glyphs: Vec<GlyphOutline>,
    pub visual_issues: Vec<VisualIssue>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DisplayCommand {
    Text(TextRun),
    Figure {
        bbox: Rect,
        resource_name: String,
        clips: Vec<ClipRegion>,
        visual_issues: Vec<VisualIssue>,
    },
    Fill {
        path: Vec<PathSegment>,
        paint: Paint,
        even_odd: bool,
        clips: Vec<ClipRegion>,
        visual_issues: Vec<VisualIssue>,
    },
    Stroke {
        path: Vec<PathSegment>,
        paint: Paint,
        style: StrokeStyle,
        clips: Vec<ClipRegion>,
        visual_issues: Vec<VisualIssue>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct FontInfo {
    pub bold: bool,
    pub base_font: String,
    decoder: FontDecoder,
    cid_widths: Option<CidWidths>,
    pub outline: Option<Arc<FontProgram>>,
    cid_identity: bool,
    cid_to_gid: Option<Arc<Vec<u16>>>,
}

#[derive(Clone, Debug)]
struct CidWidths {
    default: f32,
    widths: BTreeMap<u16, f32>,
}

#[derive(Clone, Debug)]
enum FontDecoder {
    Ascii,
    WinAnsi,
    Simple(Arc<SimpleEncoding>),
    GlyphIdentity(Arc<FontProgram>),
    ToUnicode(ToUnicodeMap),
}

#[derive(Debug)]
struct SimpleEncoding {
    characters: [Option<char>; 256],
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
    pub omitted_xobjects: bool,
    pub unmapped_text_codes: bool,
}

pub(crate) fn parse_content(
    bytes: &[u8],
    page_left: f32,
    page_height: f32,
    fonts: &BTreeMap<String, FontInfo>,
    xobjects: &BTreeSet<String>,
    ext_graphics_states: &BTreeMap<String, ExtGraphicsState>,
) -> Result<ParsedContent, DocsightError> {
    let mut error_location = ErrorLocation::default();
    parse_content_inner(
        bytes,
        page_left,
        page_height,
        fonts,
        xobjects,
        ext_graphics_states,
        &mut error_location,
    )
    .map_err(|error| error.with_error_location(error_location))
}

#[allow(clippy::too_many_arguments)]
fn parse_content_inner(
    bytes: &[u8],
    page_left: f32,
    page_height: f32,
    fonts: &BTreeMap<String, FontInfo>,
    xobjects: &BTreeSet<String>,
    ext_graphics_states: &BTreeMap<String, ExtGraphicsState>,
    error_location: &mut ErrorLocation,
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
    let mut unmapped_text_codes = false;
    let mut omitted_xobjects = false;
    let mut inline_image = false;
    loop {
        let token_offset = lexer.cursor as u64;
        error_location.operator = None;
        error_location.offset = Some(token_offset);
        let Some(token) = lexer.next_token()? else {
            break;
        };
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
                error_location.operator = Some(operator.clone());
                error_location.offset = Some(anchor_offset);
                match operator.as_str() {
                    "BX" | "EX" => {
                        require_empty(&operands, &operator)?;
                    }
                    "BI" => {
                        require_empty(&operands, &operator)?;
                        inline_image = true;
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
                            visual_issues: state.visual_issues(PaintScope::Common),
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
                        state.line_width = values[0];
                    }
                    "rg" => {
                        let values = numbers(&operands, 3, &operator)?;
                        state.fill_color_space = ColorSpace::Rgb;
                        state.fill_color = rgb(values[0], values[1], values[2])?;
                        state.fill_pattern = None;
                    }
                    "RG" => {
                        let values = numbers(&operands, 3, &operator)?;
                        state.stroke_color_space = ColorSpace::Rgb;
                        state.stroke_color = rgb(values[0], values[1], values[2])?;
                        state.stroke_pattern = None;
                    }
                    "g" => {
                        let values = numbers(&operands, 1, &operator)?;
                        state.fill_color_space = ColorSpace::Gray;
                        state.fill_color = rgb(values[0], values[0], values[0])?;
                        state.fill_pattern = None;
                    }
                    "G" => {
                        let values = numbers(&operands, 1, &operator)?;
                        state.stroke_color_space = ColorSpace::Gray;
                        state.stroke_color = rgb(values[0], values[0], values[0])?;
                        state.stroke_pattern = None;
                    }
                    "k" => {
                        let values = numbers(&operands, 4, &operator)?;
                        state.fill_color_space = ColorSpace::Cmyk;
                        state.fill_color = cmyk(values[0], values[1], values[2], values[3])?;
                        state.fill_pattern = None;
                    }
                    "K" => {
                        let values = numbers(&operands, 4, &operator)?;
                        state.stroke_color_space = ColorSpace::Cmyk;
                        state.stroke_color = cmyk(values[0], values[1], values[2], values[3])?;
                        state.stroke_pattern = None;
                    }
                    "cs" | "CS" => {
                        if operands.len() != 1 {
                            return Err(malformed(format!(
                                "{operator} requires one color-space name"
                            )));
                        }
                        let color_space = ColorSpace::parse(name(&operands[0], &operator)?);
                        if operator == "cs" {
                            state.fill_color_space = color_space;
                            state.fill_pattern = None;
                        } else {
                            state.stroke_color_space = color_space;
                            state.stroke_pattern = None;
                        }
                    }
                    "sc" | "SC" | "scn" | "SCN" => {
                        let patterns: Vec<String> = operands
                            .iter()
                            .filter_map(|value| match value {
                                ContentValue::Name(name) => Some(name.clone()),
                                _ => None,
                            })
                            .collect();
                        if !patterns.is_empty() {
                            for pattern in patterns {
                                if matches!(operator.as_str(), "sc" | "scn") {
                                    state.fill_pattern = Some(pattern);
                                } else {
                                    state.stroke_pattern = Some(pattern);
                                }
                            }
                        } else {
                            let values = operands
                                .iter()
                                .map(|value| number(value, &operator))
                                .collect::<Result<Vec<_>, _>>()?;
                            let color_space = if matches!(operator.as_str(), "sc" | "scn") {
                                &state.fill_color_space
                            } else {
                                &state.stroke_color_space
                            };
                            let color = color_space.color(&values, &operator)?;
                            if matches!(operator.as_str(), "sc" | "scn") {
                                state.fill_color = color;
                                state.fill_pattern = None;
                            } else {
                                state.stroke_color = color;
                                state.stroke_pattern = None;
                            }
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
                        if path.is_empty() {
                            return Err(malformed("h requires a current subpath"));
                        }
                        if !matches!(path.last(), Some(PathSegment::Close)) {
                            require_open_subpath(&path, &operator)?;
                            path.push(PathSegment::Close);
                        }
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
                            paint: state.stroke_paint(),
                            style: state.stroke_style(width, state.ctm.stroke_scale()?),
                            clips: state.clips.clone(),
                            visual_issues: state.visual_issues(PaintScope::Stroke),
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
                            paint: state.fill_paint(),
                            even_odd: operator == "f*",
                            clips: state.clips.clone(),
                            visual_issues: state.visual_issues(PaintScope::Fill),
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
                            path: std::mem::take(&mut path),
                            paint: state.fill_paint(),
                            even_odd,
                            clips: state.clips.clone(),
                            visual_issues: state.visual_issues(PaintScope::Fill),
                        });
                        commands.push(DisplayCommand::Stroke {
                            path: stroke_path,
                            paint: state.stroke_paint(),
                            style: state.stroke_style(width, state.ctm.stroke_scale()?),
                            clips: state.clips.clone(),
                            visual_issues: state.visual_issues(PaintScope::Stroke),
                        });
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
                        let font = fonts
                            .get(name)
                            .ok_or_else(|| malformed("Tf references an unknown font resource"))?;
                        state.text.font = Some(FontSelection {
                            size,
                            bold: font.bold,
                            font_name: font.base_font.clone(),
                            decoder: font.decoder.clone(),
                            cid_widths: font.cid_widths.clone(),
                            outline: font.outline.clone(),
                            cid_identity: font.cid_identity,
                            cid_to_gid: font.cid_to_gid.clone(),
                        });
                        approximated_font |= font.outline.is_none();
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
                        if !value.is_finite() {
                            return Err(malformed("text horizontal scale must be finite"));
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
                        state.text.render_mode = value as u8;
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
                            &mut unmapped_text_codes,
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
                                    &mut unmapped_text_codes,
                                )?,
                                ContentValue::Number(adjustment) => {
                                    let font = state
                                        .text
                                        .font
                                        .as_ref()
                                        .ok_or_else(|| malformed("TJ used before Tf"))?;
                                    state.text.matrix.e -= adjustment / 1000.0 * font.size;
                                }
                                _ => {
                                    return Err(malformed("TJ array contains an invalid item"));
                                }
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
                            &mut unmapped_text_codes,
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
                        if operator == "J" {
                            state.line_cap = line_cap(value)?;
                        } else {
                            state.line_join = line_join(value)?;
                        }
                    }
                    "M" => {
                        let value = numbers(&operands, 1, &operator)?[0];
                        if value < 1.0 {
                            return Err(malformed("miter limit must be at least one"));
                        }
                        state.miter_limit = value;
                    }
                    "d" => {
                        if operands.len() != 2 {
                            return Err(malformed("d requires a dash array and phase"));
                        }
                        let dash = match &operands[0] {
                            ContentValue::Array(values) => values,
                            _ => return Err(malformed("d requires an array dash pattern")),
                        };
                        if dash.len() > MAX_DASH_ENTRIES {
                            return Err(DocsightError::ResourceLimit {
                                resource: "PDF dash entries".to_owned(),
                                limit: MAX_DASH_ENTRIES as u64,
                            });
                        }
                        let dash = dash
                            .iter()
                            .map(|value| number(value, &operator))
                            .collect::<Result<Vec<_>, _>>()?;
                        validate_dash_pattern(&dash)?;
                        let phase = number(&operands[1], &operator)?;
                        if phase < 0.0 {
                            return Err(malformed("d dash phase must be non-negative"));
                        }
                        state.dash = dash;
                        state.dash_phase = phase;
                    }
                    "ri" | "i" => {
                        if operator == "ri" {
                            if operands.len() != 1 {
                                return Err(malformed("ri requires one intent name"));
                            }
                            validate_rendering_intent(name(&operands[0], &operator)?)?;
                        } else {
                            let value = numbers(&operands, 1, &operator)?[0];
                            if !(0.0..=100.0).contains(&value) {
                                return Err(malformed("flatness must be between zero and 100"));
                            }
                        }
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
                            visual_issues: state.visual_issues(PaintScope::Common),
                        });
                        omitted_xobjects = true;
                    }
                    "gs" => {
                        if operands.len() != 1 {
                            return Err(malformed("gs requires one graphics state name"));
                        }
                        let resource_name = name(&operands[0], &operator)?;
                        let ext_state =
                            ext_graphics_states.get(resource_name).ok_or_else(|| {
                                malformed("gs references an unknown ExtGState resource")
                            })?;
                        state.apply_ext_graphics_state(ext_state)?;
                        if let Some((font, _)) = &ext_state.font {
                            approximated_font |= font.outline.is_none();
                        }
                    }
                    "sh" => {
                        if operands.len() != 1 {
                            return Err(malformed("sh requires one shading name"));
                        }
                        let resource_name = name(&operands[0], &operator)?;
                        return Err(DocsightError::UnsupportedFeature {
                            feature: format!("PDF shading resource {resource_name}"),
                        });
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
    error_location.operator = None;
    error_location.offset = Some(bytes.len() as u64);
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
        omitted_xobjects,
        unmapped_text_codes,
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
    unmapped_text_codes: &mut bool,
) -> Result<(), DocsightError> {
    let font = state
        .text
        .font
        .as_ref()
        .ok_or_else(|| malformed("text showing operator used before Tf"))?;
    let text = font.decoder.decode(bytes)?;
    *unmapped_text_codes |= text.contains(char::REPLACEMENT_CHARACTER);
    let glyphs = match &font.outline {
        Some(program) if font.cid_identity => {
            program.glyphs_for_identity(bytes, font.cid_to_gid.as_deref().map(Vec::as_slice))?
        }
        Some(program) => program.glyphs_for_simple_text(bytes, &text)?,
        None => Vec::new(),
    };
    let character_count = text.chars().count() as f32;
    let spaces = if font.cid_widths.is_some() {
        0.0
    } else {
        bytes.iter().filter(|byte| **byte == b' ').count() as f32
    };
    let (advance, code_count) = match &font.cid_widths {
        Some(metrics) => (
            metrics.advance(bytes)? * font.size / 1000.0,
            bytes.len() as f32 / 2.0,
        ),
        None => (character_count * font.size * 0.6, character_count),
    };
    let width = advance + code_count * state.text.char_spacing + spaces * state.text.word_spacing;
    let width = width * state.text.horizontal_scale;
    if text.is_empty() || width.abs() <= f32::EPSILON {
        state.text.matrix = state.text.matrix.translated(width, 0.0);
        return Ok(());
    }
    let combined = state.text.matrix.concat(state.ctm);
    validate_text_matrix(&combined)?;
    let baseline = combined.transform(0.0, state.text.rise);
    let endpoint = combined.transform(width, state.text.rise);
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
    .map_err(|_| {
        malformed(format!(
            "text operator at content offset {anchor_offset} produced invalid geometry"
        ))
    })?;
    let alpha = (state.fill_alpha * 255.0).round() as u32;
    let argb = alpha << 24
        | u32::from(state.fill_color.red) << 16
        | u32::from(state.fill_color.green) << 8
        | u32::from(state.fill_color.blue);
    let stroke_alpha = (state.stroke_alpha * 255.0).round() as u32;
    let stroke_argb = stroke_alpha << 24
        | u32::from(state.stroke_color.red) << 16
        | u32::from(state.stroke_color.green) << 8
        | u32::from(state.stroke_color.blue);
    let stroke_scale = state.ctm.stroke_scale()?;
    let stroke_width = state.ctm.stroke_width(state.line_width)?;
    let run = TextRun {
        text,
        bbox,
        baseline_y,
        font_size: font.size.abs(),
        font_name: font.font_name.clone(),
        bold: font.bold,
        argb,
        stroke_argb,
        stroke_style: state.stroke_style(stroke_width, stroke_scale),
        render_mode: state.text.render_mode,
        mirrored_x: endpoint.x < baseline.x,
        source_offset: anchor_offset,
        source_length: anchor_length,
        clips: state.clips.clone(),
        glyphs,
        visual_issues: state.text_visual_issues(font.size),
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

#[derive(Clone, PartialEq)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Pattern,
    Other(String),
}

impl ColorSpace {
    fn parse(name: &str) -> Self {
        match name {
            "DeviceGray" | "G" => Self::Gray,
            "DeviceRGB" | "RGB" => Self::Rgb,
            "DeviceCMYK" | "CMYK" => Self::Cmyk,
            "Pattern" => Self::Pattern,
            _ => Self::Other(name.to_owned()),
        }
    }

    fn color(&self, values: &[f32], operator: &str) -> Result<Color, DocsightError> {
        match (self, values) {
            (Self::Gray, [gray]) => rgb(*gray, *gray, *gray),
            (Self::Rgb, [red, green, blue]) => rgb(*red, *green, *blue),
            (Self::Cmyk, [cyan, magenta, yellow, black]) => cmyk(*cyan, *magenta, *yellow, *black),
            (Self::Other(_), values) if values.iter().all(|value| value.is_finite()) => Ok(Color {
                red: 0,
                green: 0,
                blue: 0,
            }),
            _ => Err(malformed(format!(
                "{operator} component count does not match the active color space"
            ))),
        }
    }
}

fn line_cap(value: f32) -> Result<LineCap, DocsightError> {
    match value as u8 {
        0 => Ok(LineCap::Butt),
        1 => Ok(LineCap::Round),
        2 => Ok(LineCap::Square),
        _ => Err(malformed("line cap must be 0, 1, or 2")),
    }
}

fn line_join(value: f32) -> Result<LineJoin, DocsightError> {
    match value as u8 {
        0 => Ok(LineJoin::Miter),
        1 => Ok(LineJoin::Round),
        2 => Ok(LineJoin::Bevel),
        _ => Err(malformed("line join must be 0, 1, or 2")),
    }
}

fn validate_dash_pattern(dash: &[f32]) -> Result<(), DocsightError> {
    if dash.iter().any(|value| !value.is_finite() || *value < 0.0) {
        return Err(malformed(
            "dash array values must be finite and non-negative",
        ));
    }
    if !dash.is_empty() && dash.iter().all(|value| *value == 0.0) {
        return Err(malformed("dash array cannot contain only zero lengths"));
    }
    Ok(())
}

fn validate_rendering_intent(intent: &str) -> Result<(), DocsightError> {
    match intent {
        "AbsoluteColorimetric" | "RelativeColorimetric" | "Saturation" | "Perceptual" => Ok(()),
        _ => Err(malformed("ri references an invalid rendering intent")),
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
    if operands.is_empty() || !operands.len().is_multiple_of(2) {
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
    fill_color_space: ColorSpace,
    stroke_color_space: ColorSpace,
    fill_alpha: f32,
    stroke_alpha: f32,
    line_width: f32,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f32,
    dash: Vec<f32>,
    dash_phase: f32,
    text: TextState,
    clips: Vec<ClipRegion>,
    pending_clip: Option<ClipRegion>,
    fill_pattern: Option<String>,
    stroke_pattern: Option<String>,
    ignored_ext_keys: BTreeSet<String>,
    blend_modes: Vec<String>,
    soft_mask: bool,
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
            fill_color_space: ColorSpace::Gray,
            stroke_color_space: ColorSpace::Gray,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
            text: TextState::default(),
            clips: Vec::new(),
            pending_clip: None,
            fill_pattern: None,
            stroke_pattern: None,
            ignored_ext_keys: BTreeSet::new(),
            blend_modes: Vec::new(),
            soft_mask: false,
        }
    }
}

impl GraphicsState {
    fn visual_issues(&self, scope: PaintScope) -> Vec<VisualIssue> {
        let mut issues = self
            .ignored_ext_keys
            .iter()
            .cloned()
            .map(VisualIssue::ExtGraphicsState)
            .collect::<BTreeSet<_>>();
        if self.soft_mask {
            issues.insert(VisualIssue::SoftMask);
        }
        issues.extend(self.blend_modes.iter().cloned().map(VisualIssue::BlendMode));
        if matches!(scope, PaintScope::Fill) {
            if let ColorSpace::Other(name) = &self.fill_color_space {
                issues.insert(VisualIssue::ColorSpace(name.clone()));
            }
            if let Some(pattern) = &self.fill_pattern {
                issues.insert(VisualIssue::Pattern(pattern.clone()));
            }
        }
        if matches!(scope, PaintScope::Stroke) {
            if let ColorSpace::Other(name) = &self.stroke_color_space {
                issues.insert(VisualIssue::ColorSpace(name.clone()));
            }
            if let Some(pattern) = &self.stroke_pattern {
                issues.insert(VisualIssue::Pattern(pattern.clone()));
            }
            if self.ctm.has_non_uniform_scale() {
                issues.insert(VisualIssue::NonUniformStroke);
            }
        }
        issues.into_iter().collect()
    }

    fn text_visual_issues(&self, font_size: f32) -> Vec<VisualIssue> {
        let paints_fill = matches!(self.text.render_mode, 0 | 2 | 4 | 6);
        let paints_stroke = matches!(self.text.render_mode, 1 | 2 | 5 | 6);
        let mut issues = if paints_fill || paints_stroke {
            self.visual_issues(PaintScope::Common)
        } else {
            Vec::new()
        };
        if paints_fill {
            issues.extend(self.visual_issues(PaintScope::Fill));
        }
        if paints_stroke {
            issues.extend(self.visual_issues(PaintScope::Stroke));
        }
        if self.text.render_mode >= 4 {
            issues.push(VisualIssue::TextClip);
        }
        if font_size.is_sign_negative() && self.text.render_mode != 3 {
            issues.push(VisualIssue::NegativeFontSize);
        }
        issues.sort();
        issues.dedup();
        issues
    }

    fn fill_paint(&self) -> Paint {
        Paint {
            color: self.fill_color,
            alpha: self.fill_alpha,
        }
    }

    fn stroke_paint(&self) -> Paint {
        Paint {
            color: self.stroke_color,
            alpha: self.stroke_alpha,
        }
    }

    fn stroke_style(&self, width: f32, scale: f32) -> StrokeStyle {
        StrokeStyle {
            width,
            cap: self.line_cap,
            join: self.line_join,
            miter_limit: self.miter_limit,
            dash: self.dash.iter().map(|value| value * scale).collect(),
            dash_phase: self.dash_phase * scale,
        }
    }

    fn apply_ext_graphics_state(&mut self, state: &ExtGraphicsState) -> Result<(), DocsightError> {
        if let Some(alpha) = state.fill_alpha {
            self.fill_alpha = alpha;
        }
        if let Some(alpha) = state.stroke_alpha {
            self.stroke_alpha = alpha;
        }
        if let Some(width) = state.line_width {
            self.line_width = width;
        }
        if let Some(cap) = state.line_cap {
            self.line_cap = cap;
        }
        if let Some(join) = state.line_join {
            self.line_join = join;
        }
        if let Some(limit) = state.miter_limit {
            self.miter_limit = limit;
        }
        if let Some((dash, phase)) = &state.dash {
            self.dash.clone_from(dash);
            self.dash_phase = *phase;
        }
        if let Some((font, size)) = &state.font {
            self.text.font = Some(FontSelection {
                size: *size,
                bold: font.bold,
                font_name: font.base_font.clone(),
                decoder: font.decoder.clone(),
                cid_widths: font.cid_widths.clone(),
                outline: font.outline.clone(),
                cid_identity: font.cid_identity,
                cid_to_gid: font.cid_to_gid.clone(),
            });
        }
        if !state.ignored_keys.is_empty() {
            self.ignored_ext_keys
                .extend(state.ignored_keys.iter().cloned());
        }
        if let Some(blend_modes) = &state.blend_modes {
            self.blend_modes.clone_from(blend_modes);
        }
        if let Some(soft_mask) = state.soft_mask {
            self.soft_mask = soft_mask;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PaintScope {
    Common,
    Fill,
    Stroke,
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
    render_mode: u8,
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
            render_mode: 0,
        }
    }
}

#[derive(Clone)]
struct FontSelection {
    size: f32,
    bold: bool,
    font_name: String,
    decoder: FontDecoder,
    cid_widths: Option<CidWidths>,
    outline: Option<Arc<FontProgram>>,
    cid_identity: bool,
    cid_to_gid: Option<Arc<Vec<u16>>>,
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
    let polygons = flatten_clip_path(path)?
        .into_iter()
        .filter(|polygon| polygon.len() >= 3)
        .collect::<Vec<_>>();
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
                flatten_clip_cubic(start, *first, *second, *end, 0, &mut current)?;
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

fn flatten_clip_cubic(
    start: Point,
    first: Point,
    second: Point,
    end: Point,
    depth: u8,
    output: &mut Vec<Point>,
) -> Result<(), DocsightError> {
    if output.len() >= MAX_PATH_SEGMENTS {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF clipping path segments".to_owned(),
            limit: MAX_PATH_SEGMENTS as u64,
        });
    }
    if depth >= 12 || clip_cubic_flatness(start, first, second, end) <= 0.01 {
        output.push(end);
        return Ok(());
    }
    let start_first = point_midpoint(start, first);
    let first_second = point_midpoint(first, second);
    let second_end = point_midpoint(second, end);
    let left_second = point_midpoint(start_first, first_second);
    let right_first = point_midpoint(first_second, second_end);
    let middle = point_midpoint(left_second, right_first);
    flatten_clip_cubic(start, start_first, left_second, middle, depth + 1, output)?;
    flatten_clip_cubic(middle, right_first, second_end, end, depth + 1, output)
}

fn clip_cubic_flatness(start: Point, first: Point, second: Point, end: Point) -> f32 {
    clip_point_line_distance(first, start, end).max(clip_point_line_distance(second, start, end))
}

fn clip_point_line_distance(point: Point, start: Point, end: Point) -> f32 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        ((point.x - start.x).powi(2) + (point.y - start.y).powi(2)).sqrt()
    } else {
        (dx * (point.y - start.y) - dy * (point.x - start.x)).abs() / length
    }
}

fn point_midpoint(first: Point, second: Point) -> Point {
    Point {
        x: (first.x + second.x) / 2.0,
        y: (first.y + second.y) / 2.0,
    }
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
        let transformed = width * self.stroke_scale()?;
        if !transformed.is_finite() || transformed < 0.0 {
            return Err(malformed("stroke transform produced an invalid width"));
        }
        Ok(transformed)
    }

    fn has_non_uniform_scale(self) -> bool {
        let horizontal = (self.a * self.a + self.b * self.b).sqrt();
        let vertical = (self.c * self.c + self.d * self.d).sqrt();
        horizontal.is_finite() && vertical.is_finite() && (horizontal - vertical).abs() > 0.0001
    }

    fn stroke_scale(self) -> Result<f32, DocsightError> {
        let horizontal = (self.a * self.a + self.b * self.b).sqrt();
        let vertical = (self.c * self.c + self.d * self.d).sqrt();
        if !horizontal.is_finite() || !vertical.is_finite() {
            return Err(malformed("stroke transform produced a non-finite width"));
        }
        if (horizontal - vertical).abs() > 0.0001 {
            let mean = (horizontal * vertical).sqrt();
            if !mean.is_finite() || mean < 0.0 {
                return Err(malformed("stroke transform produced a non-finite width"));
            }
            return Ok(mean);
        }
        Ok(horizontal)
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
                match self.peek() {
                    Some(b'(') => {
                        self.parse_string()?;
                    }
                    Some(b'<') => {
                        self.parse_hex_string()?;
                    }
                    _ => self.cursor += 1,
                }
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
        if !matches!(subtype, "Type0" | "Type1" | "TrueType" | "Type3") {
            return Err(DocsightError::UnsupportedFeature {
                feature: "PDF fonts other than Type0, Type1, Type3, or TrueType".to_owned(),
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
        let base_font = match dict
            .get("BaseFont")
            .or_else(|| descendant.as_ref().and_then(|dict| dict.get("BaseFont")))
        {
            Some(Value::Name(value)) => value.as_str(),
            Some(_) => {
                return Err(malformed("BaseFont must be a name"));
            }
            None if subtype == "Type3" => resource_name.as_str(),
            None => {
                return Err(malformed("font has no BaseFont"));
            }
        };
        let canonical_font = canonical_base_font(base_font).to_owned();
        let bold = match canonical_font.as_str() {
            "Helvetica" | "Helvetica-Oblique" | "Times-Roman" | "Times-Italic" | "Courier"
            | "Courier-Oblique" | "Symbol" | "ZapfDingbats" => false,
            "Helvetica-Bold"
            | "Helvetica-BoldOblique"
            | "Times-Bold"
            | "Times-BoldItalic"
            | "Courier-Bold"
            | "Courier-BoldOblique" => true,
            _ if matches!(subtype, "Type0" | "Type1" | "Type3" | "TrueType") => {
                canonical_font.contains("Bold")
            }
            _ => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF base font {base_font}"),
                });
            }
        };
        let descriptor_value = dict.get("FontDescriptor").or_else(|| {
            descendant
                .as_ref()
                .and_then(|descendant| descendant.get("FontDescriptor"))
        });
        let outline = match descriptor_value {
            Some(value) => {
                let descriptor = resolve(value)?;
                let descriptor = match descriptor {
                    Value::Dict(descriptor) => descriptor,
                    _ => return Err(malformed("FontDescriptor must resolve to a dictionary")),
                };
                match descriptor.get("FontFile2") {
                    Some(value) => Some(Arc::new(FontProgram::parse(decode_stream_value(value)?)?)),
                    None => None,
                }
            }
            None => None,
        };
        let encoding = match dict.get("Encoding") {
            Some(value) => Some(resolve(value)?),
            None => None,
        };
        let decoder = match dict.get("ToUnicode").or_else(|| {
            descendant
                .as_ref()
                .and_then(|descendant| descendant.get("ToUnicode"))
        }) {
            Some(value) => FontDecoder::ToUnicode(parse_to_unicode(&decode_stream_value(value)?)?),
            None if subtype == "Type0" => match &outline {
                Some(program) if program.has_glyph_unicode() => {
                    FontDecoder::GlyphIdentity(Arc::clone(program))
                }
                _ => {
                    return Err(DocsightError::UnsupportedFeature {
                        feature: "Type0 PDF font without ToUnicode CMap".to_owned(),
                    });
                }
            },
            None if subtype == "TrueType" && !dict.contains_key("Encoding") => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: "TrueType PDF font without ToUnicode or an explicit encoding"
                        .to_owned(),
                });
            }
            None if subtype == "Type3" && !dict.contains_key("Encoding") => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: "Type3 PDF font without ToUnicode or an explicit encoding".to_owned(),
                });
            }
            None => decoder_from_encoding(encoding.as_ref())?,
        };
        let mut cid_to_gid = None;
        let cid_identity = match &descendant {
            Some(descendant) => {
                if !matches!(encoding.as_ref(), Some(Value::Name(name)) if name == "Identity-H") {
                    return Err(DocsightError::UnsupportedFeature {
                        feature: "CID metrics require Identity-H font encoding".to_owned(),
                    });
                }
                cid_to_gid = match descendant.get("CIDToGIDMap") {
                    None => None,
                    Some(Value::Name(name)) if name == "Identity" => None,
                    Some(value) => {
                        let table = decode_stream_value(value)?;
                        if !table.len().is_multiple_of(2) {
                            return Err(malformed(
                                "CIDToGIDMap stream must hold two bytes per CID",
                            ));
                        }
                        if table.len() / 2 > MAX_CID_TO_GID_ENTRIES {
                            return Err(DocsightError::ResourceLimit {
                                resource: "PDF CIDToGIDMap entries".to_owned(),
                                limit: MAX_CID_TO_GID_ENTRIES as u64,
                            });
                        }
                        Some(Arc::new(
                            table
                                .chunks_exact(2)
                                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                                .collect::<Vec<u16>>(),
                        ))
                    }
                };
                true
            }
            None => false,
        };
        let cid_widths = match &descendant {
            Some(descendant) => Some(CidWidths::parse(descendant, &resolve)?),
            None => None,
        };
        fonts.insert(
            resource_name,
            FontInfo {
                bold,
                base_font: canonical_font.clone(),
                decoder,
                cid_widths,
                outline,
                cid_identity,
                cid_to_gid,
            },
        );
    }
    Ok(fonts)
}

pub(crate) struct ExtGraphicsStates {
    pub states: BTreeMap<String, ExtGraphicsState>,
}

pub(crate) fn ext_graphics_states_from_resources(
    resources: &BTreeMap<String, Value>,
    resolve: impl Fn(&Value) -> Result<Value, DocsightError>,
    decode_stream_value: impl Fn(&Value) -> Result<Vec<u8>, DocsightError>,
) -> Result<ExtGraphicsStates, DocsightError> {
    let Some(resource_value) = resources.get("ExtGState") else {
        return Ok(ExtGraphicsStates {
            states: BTreeMap::new(),
        });
    };
    let resource_dict = match resolve(resource_value)? {
        Value::Dict(dict) => dict,
        _ => return Err(malformed("ExtGState resource must resolve to a dictionary")),
    };
    let mut result = BTreeMap::new();
    for (name, value) in resource_dict {
        let dict = match resolve(&value)? {
            Value::Dict(dict) => dict,
            _ => return Err(malformed("ExtGState entry must resolve to a dictionary")),
        };
        let mut state = ExtGraphicsState::default();
        for (key, value) in dict {
            match key.as_str() {
                "Type" => match resolve(&value)? {
                    Value::Name(value) if value == "ExtGState" => {}
                    _ => return Err(malformed("ExtGState Type must be ExtGState")),
                },
                "ca" => state.fill_alpha = Some(ext_alpha(&resolve(&value)?)?),
                "CA" => state.stroke_alpha = Some(ext_alpha(&resolve(&value)?)?),
                "LW" => {
                    let width = ext_number(&resolve(&value)?)?;
                    if width < 0.0 {
                        return Err(malformed("ExtGState LW must be non-negative"));
                    }
                    state.line_width = Some(width);
                }
                "LC" => {
                    let value = ext_integer(&resolve(&value)?)?;
                    state.line_cap = Some(line_cap(value as f32)?);
                }
                "LJ" => {
                    let value = ext_integer(&resolve(&value)?)?;
                    state.line_join = Some(line_join(value as f32)?);
                }
                "ML" => {
                    let limit = ext_number(&resolve(&value)?)?;
                    if limit < 1.0 {
                        return Err(malformed("ExtGState ML must be at least one"));
                    }
                    state.miter_limit = Some(limit);
                }
                "D" => state.dash = Some(ext_dash(&resolve(&value)?, &resolve)?),
                "Font" => {
                    let Value::Array(values) = resolve(&value)? else {
                        return Err(malformed("ExtGState Font must be an array"));
                    };
                    if values.len() != 2 {
                        return Err(malformed("ExtGState Font must contain a font and size"));
                    }
                    let size = ext_number(&resolve(&values[1])?)?;
                    let font_resources = BTreeMap::from([(
                        "Font".to_owned(),
                        Value::Dict(BTreeMap::from([(
                            "ExtGStateFont".to_owned(),
                            values[0].clone(),
                        )])),
                    )]);
                    let mut fonts = fonts_from_resources(
                        &font_resources,
                        |font_value| resolve(font_value),
                        |font_value| decode_stream_value(font_value),
                    )?;
                    let font = fonts
                        .remove("ExtGStateFont")
                        .ok_or_else(|| malformed("ExtGState Font could not be resolved"))?;
                    state.font = Some((font, size));
                }
                "RI" => match resolve(&value)? {
                    Value::Name(intent) => validate_rendering_intent(&intent)?,
                    _ => return Err(malformed("ExtGState RI must be a name")),
                },
                "FL" => {
                    let flatness = ext_number(&resolve(&value)?)?;
                    if !(0.0..=100.0).contains(&flatness) {
                        return Err(malformed("ExtGState FL must be between zero and 100"));
                    }
                }
                "SA" | "TK" => match resolve(&value)? {
                    Value::Bool(_) => {
                        state.ignored_keys.insert(key);
                    }
                    _ => {
                        return Err(malformed(format!("ExtGState {key} must be boolean")));
                    }
                },
                "SM" => {
                    let smoothness = ext_number(&resolve(&value)?)?;
                    if !(0.0..=1.0).contains(&smoothness) {
                        return Err(malformed("ExtGState SM must be between zero and one"));
                    }
                    state.ignored_keys.insert(key);
                }
                "HT" | "TR" | "TR2" | "BG" | "BG2" | "UCR" | "UCR2" | "FLT" => {
                    let _ = resolve(&value)?;
                    state.ignored_keys.insert(key);
                }
                "BM" => {
                    state.blend_modes = Some(non_normal_blend_modes(&resolve(&value)?)?);
                }
                "SMask" => match resolve(&value)? {
                    Value::Name(value) if value == "None" => state.soft_mask = Some(false),
                    _ => {
                        state.soft_mask = Some(true);
                    }
                },
                "AIS" => match resolve(&value)? {
                    Value::Bool(false) => {}
                    Value::Bool(true) => {
                        return Err(DocsightError::UnsupportedFeature {
                            feature: "PDF alpha-is-shape transparency".to_owned(),
                        });
                    }
                    _ => return Err(malformed("ExtGState AIS must be boolean")),
                },
                "OP" | "op" => match resolve(&value)? {
                    Value::Bool(false) => {}
                    Value::Bool(true) => {
                        state.ignored_keys.insert(key);
                    }
                    _ => return Err(malformed("ExtGState overprint value must be boolean")),
                },
                "OPM" => match ext_integer(&resolve(&value)?)? {
                    0 | 1 => {}
                    _ => return Err(malformed("ExtGState OPM must be zero or one")),
                },
                _ => {
                    return Err(DocsightError::UnsupportedFeature {
                        feature: format!("PDF ExtGState entry {key}"),
                    });
                }
            }
        }
        result.insert(name, state);
    }
    Ok(ExtGraphicsStates { states: result })
}

fn ext_number(value: &Value) -> Result<f32, DocsightError> {
    match value {
        Value::Int(value) => Ok(*value as f32),
        Value::Real(value) if value.is_finite() => Ok(*value),
        _ => Err(malformed("ExtGState value must be numeric")),
    }
}

fn ext_integer(value: &Value) -> Result<i64, DocsightError> {
    match value {
        Value::Int(value) => Ok(*value),
        _ => Err(malformed("ExtGState value must be an integer")),
    }
}

fn ext_alpha(value: &Value) -> Result<f32, DocsightError> {
    let alpha = ext_number(value)?;
    if !(0.0..=1.0).contains(&alpha) {
        return Err(malformed("ExtGState alpha must be between zero and one"));
    }
    Ok(alpha)
}

fn ext_dash(
    value: &Value,
    resolve: &impl Fn(&Value) -> Result<Value, DocsightError>,
) -> Result<(Vec<f32>, f32), DocsightError> {
    let Value::Array(values) = value else {
        return Err(malformed("ExtGState D must be an array"));
    };
    if values.len() != 2 {
        return Err(malformed("ExtGState D must contain a dash array and phase"));
    }
    let dash_values = match resolve(&values[0])? {
        Value::Array(values) => values,
        _ => return Err(malformed("ExtGState D pattern must be an array")),
    };
    if dash_values.len() > MAX_DASH_ENTRIES {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF dash entries".to_owned(),
            limit: MAX_DASH_ENTRIES as u64,
        });
    }
    let dash = dash_values
        .iter()
        .map(|value| resolve(value).and_then(|value| ext_number(&value)))
        .collect::<Result<Vec<_>, _>>()?;
    validate_dash_pattern(&dash)?;
    let phase = ext_number(&resolve(&values[1])?)?;
    if phase < 0.0 {
        return Err(malformed("ExtGState D phase must be non-negative"));
    }
    Ok((dash, phase))
}

fn non_normal_blend_modes(value: &Value) -> Result<Vec<String>, DocsightError> {
    match value {
        Value::Name(value) if value == "Normal" || value == "Compatible" => Ok(Vec::new()),
        Value::Array(values) => {
            let mut modes = Vec::new();
            for value in values {
                match value {
                    Value::Name(name) if name == "Normal" || name == "Compatible" => {}
                    Value::Name(name) => {
                        if !modes.contains(name) {
                            modes.push(name.clone());
                        }
                    }
                    _ => {
                        return Err(malformed("ExtGState BM must be a name or name array"));
                    }
                }
            }
            Ok(modes)
        }
        Value::Name(value) => Ok(vec![value.clone()]),
        _ => Err(malformed("ExtGState BM must be a name or name array")),
    }
}

impl CidWidths {
    fn parse(
        dict: &BTreeMap<String, Value>,
        resolve: &impl Fn(&Value) -> Result<Value, DocsightError>,
    ) -> Result<Self, DocsightError> {
        let default = match dict.get("DW") {
            Some(value) => font_width(&resolve(value)?)?,
            None => 1000.0,
        };
        let mut widths = BTreeMap::new();
        if let Some(value) = dict.get("W") {
            let Value::Array(values) = resolve(value)? else {
                return Err(malformed("CID W must be an array"));
            };
            let mut items = values.iter();
            while let Some(start) = items.next() {
                let start = cid_code(start)?;
                let next = items
                    .next()
                    .ok_or_else(|| malformed("incomplete CID width entry"))?;
                match next {
                    Value::Array(entries) => {
                        for (offset, width) in entries.iter().enumerate() {
                            let offset = u16::try_from(offset)
                                .map_err(|_| malformed("CID width range overflow"))?;
                            let code = start
                                .checked_add(offset)
                                .ok_or_else(|| malformed("CID width range overflow"))?;
                            if widths.insert(code, font_width(width)?).is_some() {
                                return Err(malformed("overlapping CID width ranges"));
                            }
                        }
                    }
                    end => {
                        let end = cid_code(end)?;
                        if end < start {
                            return Err(malformed("reversed CID width range"));
                        }
                        let width = font_width(
                            items
                                .next()
                                .ok_or_else(|| malformed("missing CID range width"))?,
                        )?;
                        for code in start..=end {
                            if widths.insert(code, width).is_some() {
                                return Err(malformed("overlapping CID width ranges"));
                            }
                        }
                    }
                }
            }
        }
        Ok(Self { default, widths })
    }

    fn advance(&self, bytes: &[u8]) -> Result<f32, DocsightError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(malformed(
                "Identity-H text requires two-byte character codes",
            ));
        }
        let mut width = 0.0;
        for code in bytes.chunks_exact(2) {
            let code = u16::from_be_bytes([code[0], code[1]]);
            width += self.widths.get(&code).copied().unwrap_or(self.default);
            if !width.is_finite() {
                return Err(malformed("CID text width overflow"));
            }
        }
        Ok(width)
    }
}

#[cfg(test)]
mod cid_width_tests {
    use super::*;

    #[test]
    fn resolves_array_range_and_default_widths() -> Result<(), DocsightError> {
        let dict = BTreeMap::from([
            ("DW".to_owned(), Value::Int(900)),
            (
                "W".to_owned(),
                Value::Array(vec![
                    Value::Int(1),
                    Value::Array(vec![Value::Int(300), Value::Real(450.5)]),
                    Value::Int(3),
                    Value::Int(4),
                    Value::Int(600),
                ]),
            ),
        ]);
        let widths = CidWidths::parse(&dict, &|value| Ok(value.clone()))?;
        assert_eq!(widths.advance(&[0, 1, 0, 2, 0, 3, 0, 4, 0, 5])?, 2850.5);
        assert!(widths.advance(&[0]).is_err());
        assert_eq!(
            CidWidths::parse(&BTreeMap::new(), &|value| Ok(value.clone()))?.advance(&[255, 255])?,
            1000.0
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_width_ranges() {
        for values in [
            vec![Value::Int(-1), Value::Array(vec![Value::Int(1)])],
            vec![Value::Int(65536), Value::Array(vec![Value::Int(1)])],
            vec![
                Value::Int(65535),
                Value::Array(vec![Value::Int(1), Value::Int(1)]),
            ],
            vec![Value::Int(1)],
            vec![Value::Int(2), Value::Int(1), Value::Int(300)],
            vec![Value::Int(1), Value::Int(2)],
            vec![Value::Int(1), Value::Array(vec![Value::Int(-1)])],
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(300),
                Value::Int(2),
                Value::Array(vec![Value::Int(400)]),
            ],
        ] {
            let dict = BTreeMap::from([("W".to_owned(), Value::Array(values))]);
            assert!(CidWidths::parse(&dict, &|value| Ok(value.clone())).is_err());
        }
    }
}

fn cid_code(value: &Value) -> Result<u16, DocsightError> {
    match value {
        Value::Int(value) => u16::try_from(*value).map_err(|_| malformed("CID outside 0..65535")),
        _ => Err(malformed("CID must be an integer")),
    }
}

fn font_width(value: &Value) -> Result<f32, DocsightError> {
    let width = match value {
        Value::Int(value) => *value as f32,
        Value::Real(value) => *value,
        _ => return Err(malformed("font width must be a number")),
    };
    if !width.is_finite() || width < 0.0 {
        return Err(malformed("font width must be finite and non-negative"));
    }
    Ok(width)
}

impl FontDecoder {
    fn decode(&self, bytes: &[u8]) -> Result<String, DocsightError> {
        match self {
            Self::Ascii => decode_ascii(bytes),
            Self::WinAnsi => decode_win_ansi(bytes),
            Self::Simple(encoding) => bytes
                .iter()
                .map(|byte| {
                    encoding.characters[usize::from(*byte)].ok_or_else(|| {
                        DocsightError::UnsupportedFeature {
                            feature: format!("PDF font encoding has no glyph for code {byte}"),
                        }
                    })
                })
                .collect(),
            Self::GlyphIdentity(program) => {
                if !bytes.len().is_multiple_of(2) {
                    return Err(malformed(
                        "Identity-H text requires two-byte character codes",
                    ));
                }
                Ok(bytes
                    .chunks_exact(2)
                    .filter_map(|code| {
                        program.unicode_for_glyph(u16::from_be_bytes([code[0], code[1]]))
                    })
                    .collect())
            }
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
                let skip = self.code_lengths.iter().copied().min().unwrap_or(1).max(1);
                output.push(char::REPLACEMENT_CHARACTER);
                cursor = cursor.saturating_add(skip);
                continue;
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
        Some(Value::Dict(dict)) => simple_encoding(dict),
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

const GLYPH_NAMES: &[(&str, char)] = &[
    ("A", '\u{41}'),
    ("AE", '\u{c6}'),
    ("Aacute", '\u{c1}'),
    ("Acircumflex", '\u{c2}'),
    ("Adieresis", '\u{c4}'),
    ("Agrave", '\u{c0}'),
    ("Aring", '\u{c5}'),
    ("Atilde", '\u{c3}'),
    ("B", '\u{42}'),
    ("C", '\u{43}'),
    ("Ccedilla", '\u{c7}'),
    ("D", '\u{44}'),
    ("Delta", '\u{2206}'),
    ("E", '\u{45}'),
    ("Eacute", '\u{c9}'),
    ("Ecircumflex", '\u{ca}'),
    ("Edieresis", '\u{cb}'),
    ("Egrave", '\u{c8}'),
    ("Eth", '\u{d0}'),
    ("Euro", '\u{20ac}'),
    ("F", '\u{46}'),
    ("G", '\u{47}'),
    ("H", '\u{48}'),
    ("I", '\u{49}'),
    ("Iacute", '\u{cd}'),
    ("Icircumflex", '\u{ce}'),
    ("Idieresis", '\u{cf}'),
    ("Igrave", '\u{cc}'),
    ("J", '\u{4a}'),
    ("K", '\u{4b}'),
    ("L", '\u{4c}'),
    ("Lslash", '\u{141}'),
    ("M", '\u{4d}'),
    ("N", '\u{4e}'),
    ("Ntilde", '\u{d1}'),
    ("O", '\u{4f}'),
    ("OE", '\u{152}'),
    ("Oacute", '\u{d3}'),
    ("Ocircumflex", '\u{d4}'),
    ("Odieresis", '\u{d6}'),
    ("Ograve", '\u{d2}'),
    ("Omega", '\u{3a9}'),
    ("Oslash", '\u{d8}'),
    ("Otilde", '\u{d5}'),
    ("P", '\u{50}'),
    ("Q", '\u{51}'),
    ("R", '\u{52}'),
    ("S", '\u{53}'),
    ("Scaron", '\u{160}'),
    ("T", '\u{54}'),
    ("Thorn", '\u{de}'),
    ("U", '\u{55}'),
    ("Uacute", '\u{da}'),
    ("Ucircumflex", '\u{db}'),
    ("Udieresis", '\u{dc}'),
    ("Ugrave", '\u{d9}'),
    ("V", '\u{56}'),
    ("W", '\u{57}'),
    ("X", '\u{58}'),
    ("Y", '\u{59}'),
    ("Yacute", '\u{dd}'),
    ("Ydieresis", '\u{178}'),
    ("Z", '\u{5a}'),
    ("Zcaron", '\u{17d}'),
    ("a", '\u{61}'),
    ("aacute", '\u{e1}'),
    ("acircumflex", '\u{e2}'),
    ("acute", '\u{b4}'),
    ("adieresis", '\u{e4}'),
    ("ae", '\u{e6}'),
    ("agrave", '\u{e0}'),
    ("ampersand", '\u{26}'),
    ("apple", '\u{f8ff}'),
    ("approxequal", '\u{2248}'),
    ("aring", '\u{e5}'),
    ("arrowleft", '\u{2190}'),
    ("arrowright", '\u{2192}'),
    ("asciicircum", '\u{5e}'),
    ("asciitilde", '\u{7e}'),
    ("asterisk", '\u{2a}'),
    ("at", '\u{40}'),
    ("atilde", '\u{e3}'),
    ("b", '\u{62}'),
    ("backslash", '\u{5c}'),
    ("bar", '\u{7c}'),
    ("braceleft", '\u{7b}'),
    ("braceright", '\u{7d}'),
    ("bracketleft", '\u{5b}'),
    ("bracketright", '\u{5d}'),
    ("breve", '\u{2d8}'),
    ("brokenbar", '\u{a6}'),
    ("bullet", '\u{2022}'),
    ("bullet3", '\u{2022}'),
    ("c", '\u{63}'),
    ("caron", '\u{2c7}'),
    ("ccedilla", '\u{e7}'),
    ("cedilla", '\u{b8}'),
    ("cent", '\u{a2}'),
    ("circumflex", '\u{2c6}'),
    ("colon", '\u{3a}'),
    ("comma", '\u{2c}'),
    ("copyright", '\u{a9}'),
    ("currency", '\u{a4}'),
    ("d", '\u{64}'),
    ("dagger", '\u{2020}'),
    ("daggerdbl", '\u{2021}'),
    ("degree", '\u{b0}'),
    ("dieresis", '\u{a8}'),
    ("divide", '\u{f7}'),
    ("dollar", '\u{24}'),
    ("dotaccent", '\u{2d9}'),
    ("dotlessi", '\u{131}'),
    ("e", '\u{65}'),
    ("eacute", '\u{e9}'),
    ("ecircumflex", '\u{ea}'),
    ("edieresis", '\u{eb}'),
    ("egrave", '\u{e8}'),
    ("eight", '\u{38}'),
    ("ellipsis", '\u{2026}'),
    ("emdash", '\u{2014}'),
    ("endash", '\u{2013}'),
    ("equal", '\u{3d}'),
    ("eth", '\u{f0}'),
    ("exclam", '\u{21}'),
    ("exclamdown", '\u{a1}'),
    ("f", '\u{66}'),
    ("fi", '\u{fb01}'),
    ("five", '\u{35}'),
    ("fl", '\u{fb02}'),
    ("florin", '\u{192}'),
    ("four", '\u{34}'),
    ("fraction", '\u{2044}'),
    ("g", '\u{67}'),
    ("germandbls", '\u{df}'),
    ("grave", '\u{60}'),
    ("greater", '\u{3e}'),
    ("greaterequal", '\u{2265}'),
    ("guillemotleft", '\u{ab}'),
    ("guillemotright", '\u{bb}'),
    ("guilsinglleft", '\u{2039}'),
    ("guilsinglright", '\u{203a}'),
    ("h", '\u{68}'),
    ("hungarumlaut", '\u{2dd}'),
    ("hyphen", '\u{2d}'),
    ("hyphensoft", '\u{ad}'),
    ("i", '\u{69}'),
    ("iacute", '\u{ed}'),
    ("icircumflex", '\u{ee}'),
    ("idieresis", '\u{ef}'),
    ("igrave", '\u{ec}'),
    ("infinity", '\u{221e}'),
    ("integral", '\u{222b}'),
    ("j", '\u{6a}'),
    ("k", '\u{6b}'),
    ("l", '\u{6c}'),
    ("less", '\u{3c}'),
    ("lessequal", '\u{2264}'),
    ("logicalnot", '\u{ac}'),
    ("lozenge", '\u{25ca}'),
    ("lslash", '\u{142}'),
    ("m", '\u{6d}'),
    ("macron", '\u{af}'),
    ("minus", '\u{2212}'),
    ("mu", '\u{b5}'),
    ("multiply", '\u{d7}'),
    ("n", '\u{6e}'),
    ("nbspace", '\u{a0}'),
    ("nine", '\u{39}'),
    ("notequal", '\u{2260}'),
    ("nsuperior", '\u{207f}'),
    ("ntilde", '\u{f1}'),
    ("numbersign", '\u{23}'),
    ("o", '\u{6f}'),
    ("oacute", '\u{f3}'),
    ("ocircumflex", '\u{f4}'),
    ("odieresis", '\u{f6}'),
    ("oe", '\u{153}'),
    ("ogonek", '\u{2db}'),
    ("ograve", '\u{f2}'),
    ("one", '\u{31}'),
    ("onehalf", '\u{bd}'),
    ("onequarter", '\u{bc}'),
    ("onesuperior", '\u{b9}'),
    ("ordfeminine", '\u{aa}'),
    ("ordmasculine", '\u{ba}'),
    ("oslash", '\u{f8}'),
    ("otilde", '\u{f5}'),
    ("p", '\u{70}'),
    ("paragraph", '\u{b6}'),
    ("parenleft", '\u{28}'),
    ("parenright", '\u{29}'),
    ("partialdiff", '\u{2202}'),
    ("percent", '\u{25}'),
    ("period", '\u{2e}'),
    ("periodcentered", '\u{b7}'),
    ("perthousand", '\u{2030}'),
    ("pi", '\u{3c0}'),
    ("plus", '\u{2b}'),
    ("plusminus", '\u{b1}'),
    ("product", '\u{220f}'),
    ("q", '\u{71}'),
    ("question", '\u{3f}'),
    ("questiondown", '\u{bf}'),
    ("quotedbl", '\u{22}'),
    ("quotedblbase", '\u{201e}'),
    ("quotedblleft", '\u{201c}'),
    ("quotedblright", '\u{201d}'),
    ("quoteleft", '\u{2018}'),
    ("quoteright", '\u{2019}'),
    ("quotesinglbase", '\u{201a}'),
    ("quotesingle", '\u{27}'),
    ("r", '\u{72}'),
    ("radical", '\u{221a}'),
    ("registered", '\u{ae}'),
    ("ring", '\u{2da}'),
    ("s", '\u{73}'),
    ("scaron", '\u{161}'),
    ("section", '\u{a7}'),
    ("semicolon", '\u{3b}'),
    ("seven", '\u{37}'),
    ("six", '\u{36}'),
    ("slash", '\u{2f}'),
    ("space", '\u{20}'),
    ("sterling", '\u{a3}'),
    ("summation", '\u{2211}'),
    ("t", '\u{74}'),
    ("thorn", '\u{fe}'),
    ("three", '\u{33}'),
    ("threequarters", '\u{be}'),
    ("threesuperior", '\u{b3}'),
    ("tilde", '\u{2dc}'),
    ("trademark", '\u{2122}'),
    ("trademarkserif", '\u{2122}'),
    ("two", '\u{32}'),
    ("twosuperior", '\u{b2}'),
    ("u", '\u{75}'),
    ("uacute", '\u{fa}'),
    ("ucircumflex", '\u{fb}'),
    ("udieresis", '\u{fc}'),
    ("ugrave", '\u{f9}'),
    ("underscore", '\u{5f}'),
    ("v", '\u{76}'),
    ("w", '\u{77}'),
    ("x", '\u{78}'),
    ("y", '\u{79}'),
    ("yacute", '\u{fd}'),
    ("ydieresis", '\u{ff}'),
    ("yen", '\u{a5}'),
    ("z", '\u{7a}'),
    ("zcaron", '\u{17e}'),
    ("zero", '\u{30}'),
];

fn glyph_name_character(name: &str) -> Option<char> {
    if let Some(rest) = name.strip_prefix("uni")
        && rest.len() >= 4
        && let Ok(code) = u32::from_str_radix(&rest[..4], 16)
    {
        return char::from_u32(code);
    }
    if let Some(rest) = name.strip_prefix('u')
        && (4..=6).contains(&rest.len())
        && let Ok(code) = u32::from_str_radix(rest, 16)
    {
        return char::from_u32(code);
    }
    let base = name.split_once('.').map_or(name, |(head, _)| head);
    GLYPH_NAMES
        .binary_search_by(|(candidate, _)| (*candidate).cmp(base))
        .ok()
        .map(|index| GLYPH_NAMES[index].1)
}

fn simple_encoding(dict: &BTreeMap<String, Value>) -> Result<FontDecoder, DocsightError> {
    let mut characters = [None; 256];
    match dict.get("BaseEncoding") {
        Some(Value::Name(name)) if name == "MacRomanEncoding" => {
            for (index, slot) in characters.iter_mut().enumerate() {
                *slot = mac_roman_character(index as u8);
            }
        }
        _ => {
            for (index, slot) in characters.iter_mut().enumerate() {
                *slot = win_ansi_character(index as u8).ok();
            }
        }
    }
    if let Some(value) = dict.get("Differences") {
        let Value::Array(items) = value else {
            return Err(malformed("font Encoding Differences must be an array"));
        };
        let mut code: Option<usize> = None;
        for item in items {
            match item {
                Value::Int(start) if *start >= 0 && *start < 256 => code = Some(*start as usize),
                Value::Int(_) => return Err(malformed("Differences code is out of range")),
                Value::Name(name) => {
                    let slot = code
                        .ok_or_else(|| malformed("Differences must start with a character code"))?;
                    if slot >= 256 {
                        return Err(malformed("Differences code is out of range"));
                    }
                    characters[slot] = glyph_name_character(name);
                    code = Some(slot + 1);
                }
                _ => return Err(malformed("Differences must hold codes and glyph names")),
            }
        }
    }
    Ok(FontDecoder::Simple(Arc::new(SimpleEncoding { characters })))
}

fn mac_roman_character(byte: u8) -> Option<char> {
    if byte < 0x80 {
        return char::from_u32(u32::from(byte));
    }
    crate::font::mac_roman_high(byte)
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
    let mut bytes = Vec::with_capacity(digits.len().div_ceil(2));
    let mut pairs = digits.chunks_exact(2);
    for pair in &mut pairs {
        bytes.push(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?);
    }
    if let Some(high) = pairs.remainder().first() {
        bytes.push(hex_digit(*high)? << 4);
    }
    Ok(bytes)
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
    if bytes.is_empty() {
        return Ok(String::new());
    }
    if !bytes.len().is_multiple_of(2) {
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
