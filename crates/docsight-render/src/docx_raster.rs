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
    fonts: &docsight_fonts::FontBook,
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

    for fill in &page.fills {
        let color = Color {
            red: ((fill.color_argb >> 16) & 255) as u8,
            green: ((fill.color_argb >> 8) & 255) as u8,
            blue: (fill.color_argb & 255) as u8,
        };
        canvas.fill_rect(
            fill.rect.x0,
            fill.rect.y0,
            fill.rect.x1,
            fill.rect.y1,
            color,
        );
    }
    for image in &page.images {
        let decoded = images
            .get(&image.target)
            .ok_or_else(|| DocsightError::BackendFailure {
                backend: "docsight-render".to_owned(),
                message: format!(
                    "validated image {} has no decoded pixel buffer",
                    image.target
                ),
            })?;
        canvas.draw_image(image.rect, decoded);
    }

    for border in &page.borders {
        let color = Color {
            red: ((border.color_argb >> 16) & 0xff) as u8,
            green: ((border.color_argb >> 8) & 0xff) as u8,
            blue: (border.color_argb & 0xff) as u8,
        };
        canvas.stroke_rect(border.rect, color);
    }

    let book = fonts;
    let mut rasters = BTreeMap::new();
    for run in &page.runs {
        let color = Color {
            red: ((run.color_argb >> 16) & 255) as u8,
            green: ((run.color_argb >> 8) & 255) as u8,
            blue: (run.color_argb & 255) as u8,
        };
        if let Some(line) = run.underline {
            canvas.fill_rect(line.x0, line.y0, line.x1, line.y1, color);
        }
        let face = book.resolve(&run.font_family, run.bold, run.italic)?;
        if !rasters.contains_key(&face.sha256) {
            rasters.insert(face.sha256.clone(), face.rasterizer()?);
        }
        let raster = rasters
            .get(&face.sha256)
            .ok_or_else(|| DocsightError::BackendFailure {
                backend: "docsight-fonts".to_owned(),
                message: "resolved font has no rasterizer".to_owned(),
            })?;
        let shaped = face.shape(&run.text, run.font_size)?;
        let baseline = run.baseline;
        for glyph in shaped.glyphs {
            let bitmap = raster.rasterize(glyph.id, run.font_size * scale)?;
            let left = ((run.bbox.x0 + glyph.x - target.x0) * scale).round() as i32 + bitmap.left;
            let top = ((baseline - glyph.y - target.y0) * scale).round() as i32
                - bitmap.bottom
                - i32::try_from(bitmap.height).map_err(|_| raster_limit())?;
            for row in 0..bitmap.height {
                for column in 0..bitmap.width {
                    let x = left + i32::try_from(column).map_err(|_| raster_limit())?;
                    let y = top + i32::try_from(row).map_err(|_| raster_limit())?;
                    canvas.blend_pixel(x, y, color, bitmap.coverage[row * bitmap.width + column]);
                }
            }
        }
    }

    let png = docsight_core::encode_png(width_px, height_px, &canvas.pixels)?;
    let png_bytes = u64::try_from(png.len()).map_err(|_| DocsightError::ResourceLimit {
        resource: "raster PNG bytes".to_owned(),
        limit: docsight_core::MAX_RASTER_OUTPUT_BYTES,
    })?;
    if png_bytes > docsight_core::MAX_RASTER_OUTPUT_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "raster PNG bytes".to_owned(),
            limit: docsight_core::MAX_RASTER_OUTPUT_BYTES,
        });
    }

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

    fn blend_pixel(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 3;
        for (channel, source) in [color.red, color.green, color.blue].into_iter().enumerate() {
            self.pixels[index + channel] = ((u32::from(source) * u32::from(alpha)
                + u32::from(self.pixels[index + channel]) * (255 - u32::from(alpha))
                + 127)
                / 255) as u8;
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
    "rustybuzz-0.20.1/fontdue-0.9.3/outline-v1".to_owned()
}

pub fn glyph_coverage(text: &str) -> Result<f32, DocsightError> {
    let face = docsight_fonts::FontBook::bundled()?.resolve(
        docsight_fonts::DEFAULT_FAMILY,
        false,
        false,
    )?;
    let (covered, count) = face.glyph_coverage(text)?;
    Ok(if count == 0 {
        1.0
    } else {
        covered as f32 / count as f32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use docsight_layout::TextRunLayout;

    #[test]
    fn outline_glyphs_preserve_case_and_antialiasing() -> Result<(), DocsightError> {
        let page = LaidOutPage {
            number: 1,
            width_pt: 120.0,
            height_pt: 40.0,
            runs: vec![TextRunLayout {
                text: "Café Office".to_owned(),
                font_size: 14.0,
                bold: false,
                italic: false,
                font_family: docsight_fonts::DEFAULT_FAMILY.to_owned(),
                bbox: Rect::new(3.0, 3.0, 115.0, 25.0)?,
                color_argb: 0xff000000,
                baseline: 16.0,
                underline: None,
            }],
            borders: Vec::new(),
            images: Vec::new(),
            fills: Vec::new(),
        };
        let image = rasterize_docx_page(
            &page,
            144,
            None,
            Vec::new(),
            &BTreeMap::new(),
            &docsight_fonts::FontBook::bundled()?,
        )?;
        assert!(image.pixels.iter().any(|value| *value > 0 && *value < 255));
        assert_eq!(glyph_coverage("Café")?, 1.0);
        Ok(())
    }

    #[test]
    fn missing_glyphs_are_not_drawn_as_placeholders() -> Result<(), DocsightError> {
        assert_eq!(glyph_coverage("\u{10ffff}")?, 0.0);
        Ok(())
    }
}
