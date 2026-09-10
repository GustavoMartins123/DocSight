use crate::{RenderMetadata, RenderedImage};
use docsight_core::{Diagnostic, DocsightError, Rect};
use docsight_layout::LaidOutPage;

const MIN_DPI: u16 = 36;
const MAX_DPI: u16 = 600;
const MAX_RASTER_PIXELS: u64 = 25_000_000;

#[derive(Clone, Copy)]
struct Color {
    red: u8,
    green: u8,
    blue: u8,
}

pub(crate) fn rasterize_docx_page(
    page: &LaidOutPage,
    dpi: u16,
    crop: Option<Rect>,
    warnings: Vec<Diagnostic>,
) -> Result<RenderedImage, DocsightError> {
    if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(DocsightError::InvalidArgument {
            message: format!("DPI must be between {MIN_DPI} and {MAX_DPI}"),
        });
    }
    let page_rect = Rect::new(0.0, 0.0, page.width_pt, page.height_pt)?;
    let target = match crop {
        Some(target) => {
            if target.x0 < page_rect.x0
                || target.y0 < page_rect.y0
                || target.x1 > page_rect.x1
                || target.y1 > page_rect.y1
            {
                return Err(DocsightError::InvalidArgument {
                    message: "crop rectangle must be fully inside the DOCX page".to_owned(),
                });
            }
            target
        }
        None => page_rect,
    };

    let scale = f32::from(dpi) / 72.0;
    let width_px = checked_dimension(target.width(), scale, "width")?;
    let height_px = checked_dimension(target.height(), scale, "height")?;
    let pixels = u64::from(width_px)
        .checked_mul(u64::from(height_px))
        .ok_or_else(raster_limit)?;
    if pixels > MAX_RASTER_PIXELS {
        return Err(raster_limit());
    }
    let pixel_count = usize::try_from(pixels).map_err(|_| raster_limit())?;
    let byte_len = pixel_count.checked_mul(3).ok_or_else(raster_limit)?;

    let mut canvas = Canvas {
        width: width_px,
        height: height_px,
        pixels: vec![255; byte_len],
        scale,
        offset_x: target.x0,
        offset_y: target.y0,
    };

    for border in &page.borders {
        let color = Color {
            red: ((border.color_argb >> 16) & 0xff) as u8,
            green: ((border.color_argb >> 8) & 0xff) as u8,
            blue: (border.color_argb & 0xff) as u8,
        };
        canvas.stroke_rect(border.rect, color);
    }

    for run in &page.runs {
        let color = Color {
            red: ((run.color_argb >> 16) & 0xff) as u8,
            green: ((run.color_argb >> 8) & 0xff) as u8,
            blue: (run.color_argb & 0xff) as u8,
        };
        let character_count = run.text.chars().count();
        if character_count == 0 {
            continue;
        }
        let advance = run.bbox.width() / character_count as f32;
        let cell_width = advance / 6.0;
        let cell_height = run.bbox.height() / 7.0;

        for (index, character) in run.text.chars().enumerate() {
            if character.is_whitespace()
                || character == '\u{200b}'
                || docsight_core::is_combining_mark(character)
            {
                continue;
            }
            let pattern = glyph(character).ok_or_else(|| DocsightError::UnsupportedFeature {
                feature: format!("bitmap glyph for character {character:?}"),
            })?;
            let left = run.bbox.x0 + index as f32 * advance;
            for (row, bits) in pattern.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    let x0 = left + column as f32 * cell_width;
                    let y0 = run.bbox.y0 + row as f32 * cell_height;
                    let x1 = x0 + cell_width * if run.bold { 1.35 } else { 1.0 };
                    let y1 = y0 + cell_height;
                    canvas.fill_rect(x0, y0, x1, y1, color);
                }
            }
        }
    }

    let png = docsight_core::encode_png(width_px, height_px, &canvas.pixels)?;

    Ok(RenderedImage {
        metadata: RenderMetadata {
            page: page.number,
            dpi,
            bbox: target,
            width_px,
            height_px,
            media_type: "image/png",
        },
        warnings,
        png,
        pixels: canvas.pixels,
    })
}

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
}

impl Canvas {
    fn stroke_rect(&mut self, rect: Rect, color: Color) {
        self.fill_rect(rect.x0, rect.y0, rect.x1, rect.y0 + 1.0, color);
        self.fill_rect(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1, color);
        self.fill_rect(rect.x0, rect.y0, rect.x0 + 1.0, rect.y1, color);
        self.fill_rect(rect.x1 - 1.0, rect.y0, rect.x1, rect.y1, color);
    }

    fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) {
        let px0 = ((x0 - self.offset_x) * self.scale).floor() as i32;
        let py0 = ((y0 - self.offset_y) * self.scale).floor() as i32;
        let px1 = ((x1 - self.offset_x) * self.scale).ceil() as i32;
        let py1 = ((y1 - self.offset_y) * self.scale).ceil() as i32;
        for y in py0..py1 {
            for x in px0..px1 {
                self.set_pixel(x, y, color);
            }
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 3;
        self.pixels[index] = color.red;
        self.pixels[index + 1] = color.green;
        self.pixels[index + 2] = color.blue;
    }
}

fn checked_dimension(points: f32, scale: f32, axis: &str) -> Result<u32, DocsightError> {
    let pixels = (points * scale).ceil();
    if !pixels.is_finite() || pixels <= 0.0 || pixels > u32::MAX as f32 {
        return Err(DocsightError::InvalidArgument {
            message: format!("raster {axis} is outside the supported range"),
        });
    }
    Ok(pixels as u32)
}

fn raster_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "raster pixels".to_owned(),
        limit: MAX_RASTER_PIXELS,
    }
}

pub fn raster_font_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"docsight-render-glyph-table-v1");
    for code in 32_u32..127 {
        let Some(character) = char::from_u32(code) else {
            continue;
        };
        hasher.update(code.to_le_bytes());
        if let Some(pattern) = glyph(character) {
            hasher.update(pattern);
        } else {
            hasher.update([0_u8; 7]);
        }
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn glyph_coverage(text: &str) -> f32 {
    let mut total = 0_usize;
    let mut covered = 0_usize;
    for character in text.chars() {
        if character.is_whitespace() || docsight_core::is_combining_mark(character) {
            continue;
        }
        total += 1;
        if glyph(character).is_some() {
            covered += 1;
        }
    }
    if total == 0 {
        1.0
    } else {
        covered as f32 / total as f32
    }
}

fn glyph(character: char) -> Option<[u8; 7]> {
    if let Some(pattern) = base_glyph(character) {
        return Some(pattern);
    }
    let (base, diacritic) = docsight_core::latin_decomposition(character)?;
    let base = base_glyph(base)?;
    Some(docsight_core::compose_glyph(
        base,
        diacritic,
        character.is_uppercase(),
    ))
}

fn base_glyph(character: char) -> Option<[u8; 7]> {
    let pattern = match character {
        '¹' => [4, 12, 4, 14, 0, 0, 0],
        '²' => [12, 2, 4, 14, 0, 0, 0],
        '³' => [12, 2, 4, 2, 12, 0, 0],
        '✓' => [0, 1, 2, 4, 20, 8, 0],
        'ª' => [14, 1, 15, 17, 15, 0, 31],
        'º' => [6, 9, 9, 6, 0, 15, 0],
        '●' => [0, 14, 31, 31, 31, 14, 0],
        '°' => [6, 9, 6, 0, 0, 0, 0],
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'É' => [4, 8, 31, 16, 30, 16, 31],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [28, 18, 17, 17, 17, 18, 28],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 2, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        'a' => [0, 0, 14, 1, 15, 17, 15],
        'à' => [2, 4, 14, 1, 15, 17, 15],
        'á' => [4, 2, 14, 1, 15, 17, 15],
        'ã' => [10, 0, 14, 1, 15, 17, 15],
        'b' => [16, 16, 22, 25, 17, 17, 30],
        'c' => [0, 0, 14, 17, 16, 17, 14],
        'ç' => [0, 0, 14, 17, 16, 17, 8],
        'd' => [1, 1, 13, 19, 17, 17, 15],
        'e' => [0, 0, 14, 17, 31, 16, 14],
        'é' => [4, 2, 14, 17, 31, 16, 14],
        'ê' => [10, 0, 14, 17, 31, 16, 14],
        'f' => [6, 9, 8, 28, 8, 8, 8],
        'g' => [0, 0, 15, 17, 15, 1, 14],
        'h' => [16, 16, 22, 25, 17, 17, 17],
        'i' => [4, 0, 12, 4, 4, 4, 14],
        'í' => [4, 2, 12, 4, 4, 4, 14],
        'j' => [2, 0, 6, 2, 2, 18, 12],
        'k' => [16, 16, 18, 20, 24, 20, 18],
        'l' => [12, 4, 4, 4, 4, 4, 14],
        'm' => [0, 0, 26, 21, 21, 17, 17],
        'n' => [0, 0, 22, 25, 17, 17, 17],
        'o' => [0, 0, 14, 17, 17, 17, 14],
        'ó' => [4, 2, 14, 17, 17, 17, 14],
        'ô' => [10, 0, 14, 17, 17, 17, 14],
        'õ' => [10, 0, 14, 17, 17, 17, 14],
        'p' => [0, 0, 30, 17, 30, 16, 16],
        'q' => [0, 0, 15, 17, 15, 1, 1],
        'r' => [0, 0, 22, 25, 16, 16, 16],
        's' => [0, 0, 15, 16, 14, 1, 30],
        't' => [8, 8, 28, 8, 8, 9, 6],
        'u' => [0, 0, 17, 17, 17, 19, 13],
        'ú' => [4, 2, 17, 17, 17, 19, 13],
        'v' => [0, 0, 17, 17, 17, 10, 4],
        'w' => [0, 0, 17, 17, 21, 21, 10],
        'x' => [0, 0, 17, 10, 4, 10, 17],
        'y' => [0, 0, 17, 17, 15, 1, 14],
        'z' => [0, 0, 31, 2, 4, 8, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 12, 12, 8],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '§' => [14, 16, 12, 18, 5, 3, 14],
        '×' => [0, 17, 10, 4, 10, 17, 0],
        '–' | '—' => [0, 0, 0, 31, 0, 0, 0],
        '’' | '\'' => [4, 4, 2, 0, 0, 0, 0],
        '“' | '”' => [10, 10, 5, 0, 0, 0, 0],
        '"' => [10, 10, 5, 0, 0, 0, 0],
        '•' => [0, 0, 0, 0, 12, 12, 0],
        '\u{f0b7}' => [0, 0, 0, 0, 12, 12, 0],
        '…' => [0, 0, 0, 0, 0, 21, 21],
        '→' => [0, 4, 2, 31, 2, 4, 0],
        '─' => [0, 0, 0, 31, 0, 0, 0],
        '│' => [4, 4, 4, 4, 4, 4, 4],
        '└' | '├' => [4, 4, 4, 4, 4, 4, 31],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        ';' => [0, 12, 12, 0, 12, 12, 8],
        '!' => [4, 4, 4, 4, 4, 0, 4],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '\\' => [16, 8, 8, 4, 2, 2, 1],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '{' => [6, 8, 8, 16, 8, 8, 6],
        '}' => [24, 4, 4, 2, 4, 4, 24],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '#' => [10, 31, 10, 10, 31, 10, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '&' => [12, 18, 20, 8, 21, 18, 13],
        '@' => [14, 17, 23, 21, 23, 16, 14],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '$' => [4, 14, 20, 12, 5, 11, 4],
        '~' => [0, 0, 13, 18, 0, 0, 0],
        _ => return None,
    };
    Some(pattern)
}

#[cfg(test)]
mod tests {
    use super::rasterize_docx_page;
    use docsight_core::{DocsightError, Rect};
    use docsight_layout::{LaidOutPage, TextRunLayout};

    #[test]
    fn western_european_text_renders_without_missing_glyphs() -> Result<(), DocsightError> {
        let alphabets = [
            "Ú\u{c1}\u{c0}\u{c2}\u{c3}\u{c7}\u{c9}\u{ca}\u{cd}\u{d3}\u{d4}\u{d5}\u{da}\u{dc}\u{d1}",
            "\u{e1}\u{e0}\u{e2}\u{e3}\u{e7}\u{e9}\u{ea}\u{ed}\u{f3}\u{f4}\u{f5}\u{fa}\u{fc}\u{f1}",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "abcdefghijklmnopqrstuvwxyz",
            "0123456789 \u{ba}\u{aa}\u{b2}\u{b3}\u{b9}\u{b0}",
            "linha\tcom\ttabs\ne quebra",
        ];
        for text in alphabets {
            let page = LaidOutPage {
                number: 1,
                width_pt: 720.0,
                height_pt: 72.0,
                runs: vec![TextRunLayout {
                    text: text.to_owned(),
                    font_size: 12.0,
                    bold: false,
                    bbox: Rect::new(0.0, 0.0, 700.0, 12.0)?,
                    color_argb: 0xff00_0000,
                }],
                borders: Vec::new(),
            };
            let rendered = rasterize_docx_page(&page, 72, None, Vec::new());
            assert!(
                rendered.is_ok(),
                "{text:?} must rasterize, got {:?}",
                rendered.err()
            );
        }
        Ok(())
    }

    #[test]
    fn composed_accents_keep_the_base_letter_shape() {
        let base = super::base_glyph('U').unwrap_or_default();
        let acute = super::glyph('\u{da}').unwrap_or_default();
        assert_eq!(acute[2..], [base[0], base[2], base[3], base[5], base[6]]);
        assert_ne!(acute[0..2], [0, 0]);
        assert_eq!(super::glyph('\u{c9}'), super::base_glyph('\u{c9}'));
    }

    #[test]
    fn unsupported_glyph_is_rejected() -> Result<(), DocsightError> {
        let page = LaidOutPage {
            number: 1,
            width_pt: 72.0,
            height_pt: 72.0,
            runs: vec![TextRunLayout {
                text: "🦄".to_owned(),
                font_size: 12.0,
                bold: false,
                bbox: Rect::new(0.0, 0.0, 20.0, 12.0)?,
                color_argb: 0xff00_0000,
            }],
            borders: Vec::new(),
        };
        let result = rasterize_docx_page(&page, 72, None, Vec::new());
        assert!(matches!(
            result,
            Err(DocsightError::UnsupportedFeature { feature })
                if feature == "bitmap glyph for character '🦄'"
        ));
        Ok(())
    }
}
