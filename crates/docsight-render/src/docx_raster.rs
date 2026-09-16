use crate::{RenderMetadata, RenderedImage};
use docsight_core::{DecodedImage, Diagnostic, DocsightError, Rect};
use docsight_layout::LaidOutPage;
use std::collections::BTreeMap;

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
    images: &BTreeMap<String, DecodedImage>,
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

    for image in &page.images {
        if let Some(decoded) = images.get(&image.target) {
            canvas.draw_image(image.rect, decoded);
        }
    }

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
    fn draw_image(&mut self, rect: Rect, image: &DecodedImage) {
        let left = ((rect.x0 - self.offset_x) * self.scale).round();
        let top = ((rect.y0 - self.offset_y) * self.scale).round();
        let right = ((rect.x1 - self.offset_x) * self.scale).round();
        let bottom = ((rect.y1 - self.offset_y) * self.scale).round();
        let target_width = (right - left).max(0.0) as u32;
        let target_height = (bottom - top).max(0.0) as u32;
        if target_width == 0 || target_height == 0 {
            return;
        }
        for row in 0..target_height {
            let device_y = top as i64 + i64::from(row);
            if device_y < 0 || device_y >= i64::from(self.height) {
                continue;
            }
            let source_y = u64::from(row) * u64::from(image.height) / u64::from(target_height);
            for column in 0..target_width {
                let device_x = left as i64 + i64::from(column);
                if device_x < 0 || device_x >= i64::from(self.width) {
                    continue;
                }
                let source_x = u64::from(column) * u64::from(image.width) / u64::from(target_width);
                let Some(sample) = image.pixel(source_x as u32, source_y as u32) else {
                    continue;
                };
                let Ok(index) = usize::try_from(
                    (device_y * i64::from(self.width) + device_x).saturating_mul(3),
                ) else {
                    continue;
                };
                let Some(slot) = self.pixels.get_mut(index..index + 3) else {
                    continue;
                };
                let alpha = u32::from(sample[3]);
                for channel in 0..3 {
                    let source = u32::from(sample[channel]);
                    let destination = u32::from(slot[channel]);
                    slot[channel] = ((source * alpha + destination * (255 - alpha)) / 255) as u8;
                }
            }
        }
    }

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
    hasher.update(b"docsight-render-glyph-table-v2");
    hasher.update((GLYPHS.len() as u32).to_le_bytes());
    for (character, pattern) in GLYPHS {
        hasher.update((*character as u32).to_le_bytes());
        hasher.update(pattern);
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

const GLYPHS: &[(char, [u8; 7])] = &[
    ('!', [4, 4, 4, 4, 4, 0, 4]),
    ('"', [10, 10, 5, 0, 0, 0, 0]),
    ('#', [10, 31, 10, 10, 31, 10, 0]),
    ('$', [4, 14, 20, 12, 5, 11, 4]),
    ('%', [25, 25, 2, 4, 8, 19, 19]),
    ('&', [12, 18, 20, 8, 21, 18, 13]),
    ('\'', [4, 4, 2, 0, 0, 0, 0]),
    ('(', [2, 4, 8, 8, 8, 4, 2]),
    (')', [8, 4, 2, 2, 2, 4, 8]),
    ('*', [0, 4, 21, 14, 21, 4, 0]),
    ('+', [0, 4, 4, 31, 4, 4, 0]),
    (',', [0, 0, 0, 0, 12, 12, 8]),
    ('-', [0, 0, 0, 31, 0, 0, 0]),
    ('.', [0, 0, 0, 0, 0, 12, 12]),
    ('/', [1, 2, 2, 4, 8, 8, 16]),
    ('0', [14, 17, 19, 21, 25, 17, 14]),
    ('1', [4, 12, 4, 4, 4, 4, 14]),
    ('2', [14, 17, 1, 2, 4, 8, 31]),
    ('3', [30, 1, 1, 14, 1, 1, 30]),
    ('4', [2, 6, 10, 18, 31, 2, 2]),
    ('5', [31, 16, 16, 30, 1, 1, 30]),
    ('6', [14, 16, 16, 30, 17, 17, 14]),
    ('7', [31, 1, 2, 4, 8, 8, 8]),
    ('8', [14, 17, 17, 14, 17, 17, 14]),
    ('9', [14, 17, 17, 15, 1, 1, 14]),
    (':', [0, 12, 12, 0, 12, 12, 0]),
    (';', [0, 12, 12, 0, 12, 12, 8]),
    ('<', [2, 4, 8, 16, 8, 4, 2]),
    ('=', [0, 0, 31, 0, 31, 0, 0]),
    ('>', [8, 4, 2, 1, 2, 4, 8]),
    ('?', [14, 17, 1, 2, 4, 0, 4]),
    ('@', [14, 17, 23, 21, 23, 16, 14]),
    ('A', [14, 17, 17, 31, 17, 17, 17]),
    ('B', [30, 17, 17, 30, 17, 17, 30]),
    ('C', [14, 17, 16, 16, 16, 17, 14]),
    ('D', [28, 18, 17, 17, 17, 18, 28]),
    ('E', [31, 16, 16, 30, 16, 16, 31]),
    ('F', [31, 16, 16, 30, 16, 16, 16]),
    ('G', [14, 17, 16, 23, 17, 17, 14]),
    ('H', [17, 17, 17, 31, 17, 17, 17]),
    ('I', [14, 4, 4, 4, 4, 4, 14]),
    ('J', [7, 2, 2, 2, 2, 18, 12]),
    ('K', [17, 18, 20, 24, 20, 18, 17]),
    ('L', [16, 16, 16, 16, 16, 16, 31]),
    ('M', [17, 27, 21, 21, 17, 17, 17]),
    ('N', [17, 25, 21, 19, 17, 17, 17]),
    ('O', [14, 17, 17, 17, 17, 17, 14]),
    ('P', [30, 17, 17, 30, 16, 16, 16]),
    ('Q', [14, 17, 17, 17, 21, 18, 13]),
    ('R', [30, 17, 17, 30, 20, 18, 17]),
    ('S', [15, 16, 16, 14, 1, 1, 30]),
    ('T', [31, 4, 4, 4, 4, 4, 4]),
    ('U', [17, 17, 17, 17, 17, 17, 14]),
    ('V', [17, 17, 17, 17, 17, 10, 4]),
    ('W', [17, 17, 17, 21, 21, 21, 10]),
    ('X', [17, 17, 10, 4, 10, 17, 17]),
    ('Y', [17, 17, 10, 4, 4, 4, 4]),
    ('Z', [31, 1, 2, 4, 8, 16, 31]),
    ('[', [14, 8, 8, 8, 8, 8, 14]),
    ('\\', [16, 8, 8, 4, 2, 2, 1]),
    (']', [14, 2, 2, 2, 2, 2, 14]),
    ('^', [4, 10, 17, 0, 0, 0, 0]),
    ('_', [0, 0, 0, 0, 0, 0, 31]),
    ('`', [2, 4, 0, 0, 0, 0, 0]),
    ('a', [0, 0, 14, 1, 15, 17, 15]),
    ('b', [16, 16, 22, 25, 17, 17, 30]),
    ('c', [0, 0, 14, 17, 16, 17, 14]),
    ('d', [1, 1, 13, 19, 17, 17, 15]),
    ('e', [0, 0, 14, 17, 31, 16, 14]),
    ('f', [6, 9, 8, 28, 8, 8, 8]),
    ('g', [0, 0, 15, 17, 15, 1, 14]),
    ('h', [16, 16, 22, 25, 17, 17, 17]),
    ('i', [4, 0, 12, 4, 4, 4, 14]),
    ('j', [2, 0, 6, 2, 2, 18, 12]),
    ('k', [16, 16, 18, 20, 24, 20, 18]),
    ('l', [12, 4, 4, 4, 4, 4, 14]),
    ('m', [0, 0, 26, 21, 21, 17, 17]),
    ('n', [0, 0, 22, 25, 17, 17, 17]),
    ('o', [0, 0, 14, 17, 17, 17, 14]),
    ('p', [0, 0, 30, 17, 30, 16, 16]),
    ('q', [0, 0, 15, 17, 15, 1, 1]),
    ('r', [0, 0, 22, 25, 16, 16, 16]),
    ('s', [0, 0, 15, 16, 14, 1, 30]),
    ('t', [8, 8, 28, 8, 8, 9, 6]),
    ('u', [0, 0, 17, 17, 17, 19, 13]),
    ('v', [0, 0, 17, 17, 17, 10, 4]),
    ('w', [0, 0, 17, 17, 21, 21, 10]),
    ('x', [0, 0, 17, 10, 4, 10, 17]),
    ('y', [0, 0, 17, 17, 15, 1, 14]),
    ('z', [0, 0, 31, 2, 4, 8, 31]),
    ('{', [6, 8, 8, 16, 8, 8, 6]),
    ('|', [4, 4, 4, 4, 4, 4, 4]),
    ('}', [24, 4, 4, 2, 4, 4, 24]),
    ('~', [0, 0, 13, 18, 0, 0, 0]),
    ('\u{a1}', [4, 0, 4, 4, 4, 4, 4]),
    ('\u{a2}', [4, 14, 20, 20, 20, 14, 4]),
    ('\u{a3}', [6, 9, 8, 28, 8, 8, 30]),
    ('\u{a4}', [0, 17, 14, 10, 14, 17, 0]),
    ('\u{a5}', [17, 17, 10, 31, 4, 31, 4]),
    ('\u{a6}', [4, 4, 4, 0, 4, 4, 4]),
    ('§', [14, 16, 12, 18, 5, 3, 14]),
    ('¨', [10, 0, 0, 0, 0, 0, 0]),
    ('\u{a9}', [14, 17, 21, 25, 21, 17, 14]),
    ('ª', [14, 1, 15, 17, 15, 0, 31]),
    ('\u{ab}', [0, 5, 10, 20, 10, 5, 0]),
    ('\u{ac}', [0, 0, 31, 1, 1, 0, 0]),
    ('\u{ae}', [14, 17, 29, 21, 29, 17, 14]),
    ('\u{af}', [31, 0, 0, 0, 0, 0, 0]),
    ('°', [6, 9, 6, 0, 0, 0, 0]),
    ('\u{b1}', [0, 4, 4, 31, 4, 4, 31]),
    ('²', [12, 2, 4, 14, 0, 0, 0]),
    ('³', [12, 2, 4, 2, 12, 0, 0]),
    ('´', [4, 2, 0, 0, 0, 0, 0]),
    ('\u{b5}', [0, 0, 17, 17, 17, 30, 16]),
    ('\u{b6}', [15, 21, 21, 13, 5, 5, 5]),
    ('\u{b7}', [0, 0, 0, 12, 12, 0, 0]),
    ('¸', [0, 0, 0, 0, 0, 4, 8]),
    ('¹', [4, 12, 4, 14, 0, 0, 0]),
    ('º', [6, 9, 9, 6, 0, 15, 0]),
    ('\u{bb}', [0, 20, 10, 5, 10, 20, 0]),
    ('\u{bc}', [8, 8, 4, 2, 9, 15, 1]),
    ('\u{bd}', [8, 8, 4, 2, 11, 1, 3]),
    ('\u{be}', [24, 8, 4, 2, 9, 15, 1]),
    ('\u{bf}', [4, 0, 4, 8, 16, 17, 14]),
    ('\u{c6}', [7, 12, 20, 23, 28, 20, 23]),
    ('É', [4, 8, 31, 16, 30, 16, 31]),
    ('\u{d0}', [28, 18, 17, 31, 17, 18, 28]),
    ('×', [0, 17, 10, 4, 10, 17, 0]),
    ('\u{d8}', [14, 19, 21, 21, 25, 17, 14]),
    ('\u{de}', [16, 30, 17, 17, 30, 16, 16]),
    ('\u{df}', [12, 18, 18, 28, 18, 18, 28]),
    ('à', [2, 4, 14, 1, 15, 17, 15]),
    ('á', [4, 2, 14, 1, 15, 17, 15]),
    ('ã', [10, 0, 14, 1, 15, 17, 15]),
    ('\u{e6}', [0, 0, 26, 5, 15, 20, 11]),
    ('ç', [0, 0, 14, 17, 16, 17, 8]),
    ('é', [4, 2, 14, 17, 31, 16, 14]),
    ('ê', [10, 0, 14, 17, 31, 16, 14]),
    ('í', [4, 2, 12, 4, 4, 4, 14]),
    ('\u{f0}', [10, 4, 14, 1, 15, 17, 14]),
    ('ó', [4, 2, 14, 17, 17, 17, 14]),
    ('ô', [10, 0, 14, 17, 17, 17, 14]),
    ('õ', [10, 0, 14, 17, 17, 17, 14]),
    ('\u{f7}', [0, 4, 0, 31, 0, 4, 0]),
    ('\u{f8}', [0, 0, 15, 19, 21, 25, 30]),
    ('ú', [4, 2, 17, 17, 17, 19, 13]),
    ('\u{fe}', [16, 16, 30, 17, 17, 30, 16]),
    ('\u{131}', [0, 0, 12, 4, 4, 4, 14]),
    ('\u{152}', [15, 20, 20, 23, 20, 20, 15]),
    ('\u{153}', [0, 0, 26, 21, 23, 20, 11]),
    ('\u{160}', [10, 4, 15, 16, 14, 1, 30]),
    ('\u{161}', [10, 4, 15, 16, 14, 1, 30]),
    ('\u{178}', [10, 0, 17, 10, 4, 4, 4]),
    ('\u{17d}', [10, 4, 31, 2, 4, 16, 31]),
    ('\u{17e}', [10, 4, 31, 2, 4, 8, 31]),
    ('\u{192}', [3, 4, 4, 14, 4, 4, 24]),
    ('ˆ', [4, 10, 0, 0, 0, 0, 0]),
    ('\u{2c7}', [10, 4, 0, 0, 0, 0, 0]),
    ('\u{2d8}', [17, 14, 0, 0, 0, 0, 0]),
    ('\u{2d9}', [4, 0, 0, 0, 0, 0, 0]),
    ('\u{2da}', [4, 10, 4, 0, 0, 0, 0]),
    ('\u{2db}', [0, 0, 0, 0, 0, 4, 12]),
    ('˜', [10, 0, 0, 0, 0, 0, 0]),
    ('\u{2dd}', [18, 9, 0, 0, 0, 0, 0]),
    ('\u{3a9}', [14, 17, 17, 17, 10, 10, 27]),
    ('\u{3c0}', [0, 0, 31, 10, 10, 10, 19]),
    ('–', [0, 0, 0, 31, 0, 0, 0]),
    ('—', [0, 0, 0, 31, 0, 0, 0]),
    ('\u{2018}', [4, 4, 8, 0, 0, 0, 0]),
    ('’', [4, 4, 2, 0, 0, 0, 0]),
    ('\u{201a}', [0, 0, 0, 0, 4, 4, 8]),
    ('“', [10, 10, 5, 0, 0, 0, 0]),
    ('”', [10, 10, 5, 0, 0, 0, 0]),
    ('\u{201e}', [0, 0, 0, 0, 10, 10, 20]),
    ('\u{2020}', [4, 14, 4, 4, 4, 4, 0]),
    ('\u{2021}', [4, 14, 4, 14, 4, 4, 0]),
    ('•', [0, 0, 0, 0, 12, 12, 0]),
    ('…', [0, 0, 0, 0, 0, 21, 21]),
    ('\u{2030}', [17, 2, 4, 8, 21, 0, 5]),
    ('\u{2039}', [0, 2, 4, 8, 4, 2, 0]),
    ('\u{203a}', [0, 8, 4, 2, 4, 8, 0]),
    ('\u{2044}', [1, 2, 2, 4, 8, 8, 16]),
    ('\u{20ac}', [6, 9, 28, 8, 28, 9, 6]),
    ('\u{2122}', [27, 21, 17, 0, 0, 0, 0]),
    ('→', [0, 4, 2, 31, 2, 4, 0]),
    ('\u{2202}', [14, 1, 1, 15, 17, 17, 14]),
    ('\u{2206}', [0, 4, 4, 10, 10, 17, 31]),
    ('\u{220f}', [31, 10, 10, 10, 10, 10, 27]),
    ('\u{2211}', [31, 16, 8, 4, 8, 16, 31]),
    ('\u{2217}', [0, 0, 21, 14, 21, 0, 0]),
    ('\u{221a}', [3, 2, 2, 18, 10, 4, 0]),
    ('\u{221e}', [0, 0, 10, 21, 21, 10, 0]),
    ('\u{222b}', [3, 4, 4, 4, 4, 4, 24]),
    ('\u{2248}', [0, 13, 18, 0, 13, 18, 0]),
    ('\u{2260}', [2, 2, 31, 4, 31, 8, 8]),
    ('\u{2264}', [2, 4, 8, 4, 2, 0, 31]),
    ('\u{2265}', [8, 4, 2, 4, 8, 0, 31]),
    ('─', [0, 0, 0, 31, 0, 0, 0]),
    ('│', [4, 4, 4, 4, 4, 4, 4]),
    ('└', [4, 4, 4, 4, 4, 4, 31]),
    ('├', [4, 4, 4, 4, 4, 4, 31]),
    ('\u{25ca}', [4, 10, 17, 17, 17, 10, 4]),
    ('●', [0, 14, 31, 31, 31, 14, 0]),
    ('✓', [0, 1, 2, 4, 20, 8, 0]),
    ('\u{f0b7}', [0, 0, 0, 0, 12, 12, 0]),
    ('\u{fb01}', [6, 9, 29, 8, 8, 8, 9]),
    ('\u{fb02}', [6, 9, 29, 8, 8, 8, 11]),
    ('\u{fffd}', [31, 17, 21, 17, 21, 17, 31]),
];

fn base_glyph(character: char) -> Option<[u8; 7]> {
    GLYPHS
        .binary_search_by(|(candidate, _)| candidate.cmp(&character))
        .ok()
        .map(|index| GLYPHS[index].1)
}

#[cfg(test)]
mod tests {
    use super::rasterize_docx_page;
    use docsight_core::{DocsightError, Rect};
    use docsight_layout::{LaidOutPage, TextRunLayout};

    #[test]
    fn composed_accents_keep_the_base_letter_shape() {
        let base = super::base_glyph('U').unwrap_or_default();
        let acute = super::glyph('\u{da}').unwrap_or_default();
        assert_eq!(acute[2..], [base[0], base[2], base[3], base[5], base[6]]);
        assert_ne!(acute[0..2], [0, 0]);
        assert_eq!(super::glyph('\u{c9}'), super::base_glyph('\u{c9}'));
    }

    #[test]
    fn glyph_table_is_sorted_and_unique() {
        let mut previous: Option<char> = None;
        for (character, _) in super::GLYPHS {
            if let Some(previous) = previous {
                assert!(
                    previous < *character,
                    "GLYPHS must be sorted and free of duplicates: {previous:?} then {character:?}"
                );
            }
            previous = Some(*character);
        }
        for (character, pattern) in super::GLYPHS {
            assert_eq!(super::base_glyph(*character), Some(*pattern));
        }
        assert_eq!(super::raster_font_fingerprint().len(), 64);
    }

    #[test]
    fn required_repertoire_is_supported() {
        let required = [
            "\u{fffd}",
            "\u{21}\u{22}\u{23}\u{24}\u{25}\u{26}\u{27}\u{28}\u{29}\u{2a}\u{2b}\u{2c}\u{2d}\u{2e}\u{2f}\u{30}\u{31}\u{32}\u{33}\u{34}\u{35}\u{36}\u{37}\u{38}\u{39}\u{3a}\u{3b}\u{3c}\u{3d}\u{3e}\u{3f}\u{40}\u{41}\u{42}\u{43}\u{44}\u{45}\u{46}\u{47}\u{48}",
            "\u{49}\u{4a}\u{4b}\u{4c}\u{4d}\u{4e}\u{4f}\u{50}\u{51}\u{52}\u{53}\u{54}\u{55}\u{56}\u{57}\u{58}\u{59}\u{5a}\u{5b}\u{5c}\u{5d}\u{5e}\u{5f}\u{60}\u{61}\u{62}\u{63}\u{64}\u{65}\u{66}\u{67}\u{68}\u{69}\u{6a}\u{6b}\u{6c}\u{6d}\u{6e}\u{6f}\u{70}",
            "\u{71}\u{72}\u{73}\u{74}\u{75}\u{76}\u{77}\u{78}\u{79}\u{7a}\u{7b}\u{7c}\u{7d}\u{7e}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{ba}\u{bb}",
            "\u{bc}\u{bd}\u{be}\u{bf}\u{c0}\u{c1}\u{c2}\u{c3}\u{c4}\u{c5}\u{c6}\u{c7}\u{c8}\u{c9}\u{ca}\u{cb}\u{cc}\u{cd}\u{ce}\u{cf}\u{d0}\u{d1}\u{d2}\u{d3}\u{d4}\u{d5}\u{d6}\u{d7}\u{d8}\u{d9}\u{da}\u{db}\u{dc}\u{dd}\u{de}\u{df}\u{e0}\u{e1}\u{e2}\u{e3}",
            "\u{e4}\u{e5}\u{e6}\u{e7}\u{e8}\u{e9}\u{ea}\u{eb}\u{ec}\u{ed}\u{ee}\u{ef}\u{f0}\u{f1}\u{f2}\u{f3}\u{f4}\u{f5}\u{f6}\u{f7}\u{f8}\u{f9}\u{fa}\u{fb}\u{fc}\u{fd}\u{fe}\u{ff}\u{131}\u{152}\u{153}\u{160}\u{161}\u{178}\u{17d}\u{17e}\u{192}\u{2c6}\u{2c7}\u{2d8}",
            "\u{2d9}\u{2da}\u{2db}\u{2dc}\u{2dd}\u{3a9}\u{3c0}\u{2013}\u{2014}\u{2018}\u{2019}\u{201a}\u{201c}\u{201d}\u{201e}\u{2020}\u{2021}\u{2022}\u{2026}\u{2030}\u{2039}\u{203a}\u{2044}\u{20ac}\u{2122}\u{2202}\u{2206}\u{220f}\u{2211}\u{221a}\u{221e}\u{222b}\u{2248}\u{2260}\u{2264}\u{2265}\u{25ca}\u{fb01}\u{fb02}",
        ];
        let missing: Vec<char> = required
            .iter()
            .flat_map(|group| group.chars())
            .filter(|character| super::glyph(*character).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "the DOCX raster font cannot draw every character its encodings can produce: {missing:?}"
        );
    }

    #[test]
    fn western_european_text_renders_without_missing_glyphs() -> Result<(), DocsightError> {
        let samples = [
            "\u{c1}\u{c0}\u{c2}\u{c3}\u{c7}\u{c9}\u{ca}\u{cd}\u{d3}\u{d4}\u{d5}\u{da}\u{dc}\u{d1}",
            "\u{e1}\u{e0}\u{e2}\u{e3}\u{e7}\u{e9}\u{ea}\u{ed}\u{f3}\u{f4}\u{f5}\u{fa}\u{fc}\u{f1}",
            "traco \u{2013} aspas \u{201c}assim\u{201d} bullet \u{2022}",
            "linha\tcom\ttabs\ne quebra",
        ];
        for text in samples {
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
                images: Vec::new(),
            };
            let rendered = rasterize_docx_page(
                &page,
                72,
                None,
                Vec::new(),
                &std::collections::BTreeMap::new(),
            );
            assert!(rendered.is_ok(), "{text:?} -> {:?}", rendered.err());
        }
        Ok(())
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
            images: Vec::new(),
        };
        let result = rasterize_docx_page(
            &page,
            72,
            None,
            Vec::new(),
            &std::collections::BTreeMap::new(),
        );
        assert!(matches!(
            result,
            Err(DocsightError::UnsupportedFeature { feature })
                if feature == "bitmap glyph for character '🦄'"
        ));
        Ok(())
    }
}
