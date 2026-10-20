use docsight_core::DocsightError;
use docsight_fonts::{DEFAULT_FAMILY, FontBook, FontFace};

pub fn font_fingerprint() -> Result<String, DocsightError> {
    Ok(FontBook::normalized()?.fingerprint())
}

pub fn char_width(character: char, size_pt: f32) -> Result<f32, DocsightError> {
    text_width(&character.to_string(), size_pt)
}

pub fn text_width(text: &str, size_pt: f32) -> Result<f32, DocsightError> {
    Ok(FontBook::normalized()?
        .resolve(DEFAULT_FAMILY, false, false)?
        .shape(text, size_pt)?
        .width)
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedLine {
    pub text: String,
    pub start_char: usize,
}

pub fn wrap_text(text: &str, size_pt: f32, max_width: f32) -> Result<Vec<String>, DocsightError> {
    Ok(wrap_text_indexed(text, size_pt, max_width, max_width)?
        .into_iter()
        .map(|line| line.text)
        .collect())
}

pub fn wrap_text_indexed(
    text: &str,
    size_pt: f32,
    first_width: f32,
    width: f32,
) -> Result<Vec<WrappedLine>, DocsightError> {
    let face = FontBook::normalized()?.resolve(DEFAULT_FAMILY, false, false)?;
    wrap_with_face(text, size_pt, first_width, width, &face)
}

pub(crate) fn wrap_with_face(
    text: &str,
    size: f32,
    first_width: f32,
    width: f32,
    face: &FontFace,
) -> Result<Vec<WrappedLine>, DocsightError> {
    if !width.is_finite() || width <= 0.0 || !first_width.is_finite() || first_width <= 0.0 {
        return Err(DocsightError::MalformedDocument {
            message: "text line widths must be finite and positive".to_owned(),
        });
    }
    let mut lines = Vec::new();
    let mut char_start = 0;
    for segment in text.split('\n') {
        let characters: Vec<char> = segment.chars().collect();
        let offsets: Vec<usize> = segment.char_indices().map(|(offset, _)| offset).collect();
        let mut advances = vec![0.0_f32; characters.len()];
        for glyph in face.shape(segment, size)?.glyphs {
            let index = offsets
                .partition_point(|offset| *offset <= glyph.cluster as usize)
                .saturating_sub(1);
            if let Some(advance) = advances.get_mut(index) {
                *advance += glyph.advance;
            }
        }
        let mut start = 0;
        if characters.is_empty() {
            lines.push(WrappedLine {
                text: String::new(),
                start_char: char_start,
            });
        }
        while start < characters.len() {
            let limit = if lines.is_empty() { first_width } else { width };
            let mut end = start;
            let mut last_break = None;
            let mut measured_width = 0.0;
            while end < characters.len() {
                if measured_width + advances[end] > limit && end > start {
                    break;
                }
                if characters[end].is_whitespace() && characters[end] != '\u{a0}' {
                    last_break = Some(end + 1);
                }
                measured_width += advances[end];
                end += 1;
            }
            if end < characters.len()
                && let Some(boundary) = last_break
            {
                end = boundary;
            }
            lines.push(WrappedLine {
                text: characters[start..end].iter().collect(),
                start_char: char_start + start,
            });
            start = end;
        }
        char_start += characters.len() + 1;
    }
    Ok(lines)
}
