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
            if character == ' ' {
                continue;
            }
            let pattern = glyph(character);
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

    let png = encode_png(width_px, height_px, &canvas.pixels)?;

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

pub fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let pixel_width = usize::try_from(width).map_err(|_| raster_limit())?;
    let row_bytes = pixel_width.checked_mul(3).ok_or_else(raster_limit)?;
    let raw_capacity = row_bytes
        .checked_add(1)
        .and_then(|value| value.checked_mul(height as usize))
        .ok_or_else(raster_limit)?;
    let mut raw = Vec::with_capacity(raw_capacity);
    for row in pixels.chunks_exact(row_bytes) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let compressed = zlib_store(&raw)?;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header)?;
    append_chunk(&mut png, b"IDAT", &compressed)?;
    append_chunk(&mut png, b"IEND", &[])?;
    Ok(png)
}

fn zlib_store(bytes: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut output = vec![0x78, 0x01];
    if bytes.is_empty() {
        output.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    } else {
        let chunks = bytes.chunks(u16::MAX as usize);
        let count = chunks.len();
        for (index, chunk) in chunks.enumerate() {
            output.push(if index + 1 == count { 1 } else { 0 });
            let length = u16::try_from(chunk.len()).map_err(|_| raster_limit())?;
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(&(!length).to_le_bytes());
            output.extend_from_slice(chunk);
        }
    }
    output.extend_from_slice(&adler32(bytes).to_be_bytes());
    Ok(output)
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), DocsightError> {
    let length = u32::try_from(data.len()).map_err(|_| raster_limit())?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut checksum_input = Vec::with_capacity(kind.len() + data.len());
    checksum_input.extend_from_slice(kind);
    checksum_input.extend_from_slice(data);
    output.extend_from_slice(&crc32(&checksum_input).to_be_bytes());
    Ok(())
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut first = 1u32;
    let mut second = 0u32;
    for byte in bytes {
        first = (first + u32::from(*byte)) % 65_521;
        second = (second + first) % 65_521;
    }
    second << 16 | first
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut value = 0xffff_ffffu32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(value & 1);
            value = (value >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !value
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
        hasher.update(glyph(character));
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
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
        'b' => [16, 16, 22, 25, 17, 17, 30],
        'c' => [0, 0, 14, 17, 16, 17, 14],
        'd' => [1, 1, 13, 19, 17, 17, 15],
        'e' => [0, 0, 14, 17, 31, 16, 14],
        'f' => [6, 9, 8, 28, 8, 8, 8],
        'g' => [0, 0, 15, 17, 15, 1, 14],
        'h' => [16, 16, 22, 25, 17, 17, 17],
        'i' => [4, 0, 12, 4, 4, 4, 14],
        'j' => [2, 0, 6, 2, 2, 18, 12],
        'k' => [16, 16, 18, 20, 24, 20, 18],
        'l' => [12, 4, 4, 4, 4, 4, 14],
        'm' => [0, 0, 26, 21, 21, 17, 17],
        'n' => [0, 0, 22, 25, 17, 17, 17],
        'o' => [0, 0, 14, 17, 17, 17, 14],
        'p' => [0, 0, 30, 17, 30, 16, 16],
        'q' => [0, 0, 15, 17, 15, 1, 1],
        'r' => [0, 0, 22, 25, 16, 16, 16],
        's' => [0, 0, 15, 16, 14, 1, 30],
        't' => [8, 8, 28, 8, 8, 9, 6],
        'u' => [0, 0, 17, 17, 17, 19, 13],
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
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '\'' => [4, 4, 2, 0, 0, 0, 0],
        '"' => [10, 10, 5, 0, 0, 0, 0],
        '#' => [10, 31, 10, 10, 31, 10, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '&' => [12, 18, 20, 8, 21, 18, 13],
        '@' => [14, 17, 23, 21, 23, 16, 14],
        _ => [31, 17, 17, 17, 17, 17, 31],
    }
}
