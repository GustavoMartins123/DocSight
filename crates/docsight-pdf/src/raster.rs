use crate::content::{Color, DisplayCommand, PathSegment, Point, TextRun};
use docsight_core::{DocsightError, Rect};

pub const MIN_DPI: u16 = 36;
pub const MAX_DPI: u16 = 600;
pub const MAX_RASTER_PIXELS: u64 = 25_000_000;

pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
    pub pixels: Vec<u8>,
}

pub(crate) fn rasterize(
    commands: &[DisplayCommand],
    page: Rect,
    target: Rect,
    dpi: u16,
) -> Result<Raster, DocsightError> {
    if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(DocsightError::InvalidArgument {
            message: format!("DPI must be between {MIN_DPI} and {MAX_DPI}"),
        });
    }
    if target.x0 < page.x0 || target.y0 < page.y0 || target.x1 > page.x1 || target.y1 > page.y1 {
        return Err(DocsightError::InvalidArgument {
            message: "crop rectangle must be fully inside the PDF page".to_owned(),
        });
    }
    let scale = f32::from(dpi) / 72.0;
    let width = checked_dimension(target.width(), scale, "width")?;
    let height = checked_dimension(target.height(), scale, "height")?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(raster_limit)?;
    if pixels > MAX_RASTER_PIXELS {
        return Err(raster_limit());
    }
    let pixel_count = usize::try_from(pixels).map_err(|_| raster_limit())?;
    let byte_len = pixel_count.checked_mul(3).ok_or_else(raster_limit)?;
    let mut canvas = Canvas {
        width,
        height,
        pixels: vec![255; byte_len],
        scale,
        offset_x: target.x0,
        offset_y: target.y0,
    };
    for command in commands {
        match command {
            DisplayCommand::Text(run) => canvas.draw_text(run)?,
            DisplayCommand::Fill { path, color } => canvas.fill_path(path, *color)?,
            DisplayCommand::Stroke { path, color, width } => {
                canvas.stroke_path(path, *color, *width)?;
            }
        }
    }
    Ok(Raster {
        width,
        height,
        png: encode_png(width, height, &canvas.pixels)?,
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
    fn draw_text(&mut self, run: &TextRun) -> Result<(), DocsightError> {
        let character_count = run.text.chars().count();
        if character_count == 0 {
            return Ok(());
        }
        let advance = run.bbox.width() / character_count as f32;
        let color = Color {
            red: ((run.argb >> 16) & 0xff) as u8,
            green: ((run.argb >> 8) & 0xff) as u8,
            blue: (run.argb & 0xff) as u8,
        };
        for (index, character) in run.text.chars().enumerate() {
            if character == ' ' {
                continue;
            }
            let pattern = glyph(character).ok_or_else(|| DocsightError::UnsupportedFeature {
                feature: format!("bitmap glyph for character {character:?}"),
            })?;
            let left = run.bbox.x0 + index as f32 * advance;
            let cell_width = advance / 6.0;
            let cell_height = run.bbox.height() / 7.0;
            for (row, bits) in pattern.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    let x0 = left + column as f32 * cell_width;
                    let y0 = run.bbox.y0 + row as f32 * cell_height;
                    let x1 = x0 + cell_width * if run.bold { 1.35 } else { 1.0 };
                    let y1 = y0 + cell_height;
                    self.fill_rect(x0, y0, x1, y1, color);
                }
            }
        }
        Ok(())
    }

    fn fill_path(&mut self, path: &[PathSegment], color: Color) -> Result<(), DocsightError> {
        let polygons = flatten_subpaths(path)?;
        if polygons.len() != 1 {
            return Err(DocsightError::UnsupportedFeature {
                feature: "compound PDF fill paths".to_owned(),
            });
        }
        let mut polygon = match polygons.into_iter().next() {
            Some(polygon) => polygon,
            None => {
                return Err(DocsightError::MalformedDocument {
                    message: "filled PDF path has no subpath".to_owned(),
                });
            }
        };
        if polygon.len() < 3 {
            return Err(DocsightError::MalformedDocument {
                message: "filled PDF path has fewer than three points".to_owned(),
            });
        }
        if polygon.first() != polygon.last() {
            let first = polygon[0];
            polygon.push(first);
        }
        self.fill_polygon(&polygon, color);
        Ok(())
    }

    fn stroke_path(
        &mut self,
        path: &[PathSegment],
        color: Color,
        width: f32,
    ) -> Result<(), DocsightError> {
        for points in flatten_subpaths(path)? {
            for pair in points.windows(2) {
                self.stroke_line(pair[0], pair[1], color, width);
            }
        }
        Ok(())
    }

    fn fill_polygon(&mut self, polygon: &[Point], color: Color) {
        let min_x = polygon
            .iter()
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = polygon
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        let max_x = polygon
            .iter()
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = polygon
            .iter()
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let x0 = self.pixel_x(min_x).max(0);
        let y0 = self.pixel_y(min_y).max(0);
        let x1 = self.pixel_x(max_x).min(self.width as i32);
        let y1 = self.pixel_y(max_y).min(self.height as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let point = Point {
                    x: (x as f32 + 0.5) / self.scale + self.offset_x,
                    y: (y as f32 + 0.5) / self.scale + self.offset_y,
                };
                if winding_number(point, polygon) != 0 {
                    self.set_pixel(x, y, color);
                }
            }
        }
    }

    fn stroke_line(&mut self, start: Point, end: Point, color: Color, width: f32) {
        let x0 = self.pixel_x(start.x);
        let y0 = self.pixel_y(start.y);
        let x1 = self.pixel_x(end.x);
        let y1 = self.pixel_y(end.y);
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let mut x = x0;
        let mut y = y0;
        let radius = ((width * self.scale / 2.0).ceil() as i32).max(1);
        loop {
            for offset_y in -radius..=radius {
                for offset_x in -radius..=radius {
                    self.set_pixel(x + offset_x, y + offset_y, color);
                }
            }
            if x == x1 && y == y1 {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) {
        let left = self.pixel_x(x0).max(0);
        let top = self.pixel_y(y0).max(0);
        let right = self.pixel_x(x1).min(self.width as i32);
        let bottom = self.pixel_y(y1).min(self.height as i32);
        for y in top..bottom {
            for x in left..right {
                self.set_pixel(x, y, color);
            }
        }
    }

    fn pixel_x(&self, value: f32) -> i32 {
        ((value - self.offset_x) * self.scale).round() as i32
    }

    fn pixel_y(&self, value: f32) -> i32 {
        ((value - self.offset_y) * self.scale).round() as i32
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

fn flatten_subpaths(path: &[PathSegment]) -> Result<Vec<Vec<Point>>, DocsightError> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut cursor = None;
    for segment in path {
        match segment {
            PathSegment::Move(point) => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
                current.push(*point);
                cursor = Some(*point);
            }
            PathSegment::Line(point) => {
                if cursor.is_none() {
                    return Err(malformed_path("line segment has no starting point"));
                }
                current.push(*point);
                cursor = Some(*point);
            }
            PathSegment::Cubic(control1, control2, end) => {
                let start = cursor.ok_or_else(|| malformed_path("curve has no starting point"))?;
                for step in 1..=24 {
                    let t = step as f32 / 24.0;
                    current.push(cubic_point(start, *control1, *control2, *end, t));
                }
                cursor = Some(*end);
            }
            PathSegment::Close => {
                if let Some(first) = current.first().copied() {
                    if current.last().copied() != Some(first) {
                        current.push(first);
                    }
                }
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}

fn cubic_point(start: Point, first: Point, second: Point, end: Point, t: f32) -> Point {
    let inverse = 1.0 - t;
    Point {
        x: inverse.powi(3) * start.x
            + 3.0 * inverse.powi(2) * t * first.x
            + 3.0 * inverse * t.powi(2) * second.x
            + t.powi(3) * end.x,
        y: inverse.powi(3) * start.y
            + 3.0 * inverse.powi(2) * t * first.y
            + 3.0 * inverse * t.powi(2) * second.y
            + t.powi(3) * end.y,
    }
}

fn winding_number(point: Point, polygon: &[Point]) -> i32 {
    polygon.windows(2).fold(0, |winding, edge| {
        let first = edge[0];
        let second = edge[1];
        if first.y <= point.y && second.y > point.y && is_left(first, second, point) > 0.0 {
            winding + 1
        } else if first.y > point.y && second.y <= point.y && is_left(first, second, point) < 0.0 {
            winding - 1
        } else {
            winding
        }
    })
}

fn is_left(first: Point, second: Point, point: Point) -> f32 {
    (second.x - first.x) * (point.y - first.y) - (point.x - first.x) * (second.y - first.y)
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

fn malformed_path(message: &str) -> DocsightError {
    DocsightError::MalformedDocument {
        message: message.to_owned(),
    }
}

fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, DocsightError> {
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
            value = value >> 1 ^ 0xedb8_8320 & mask;
        }
    }
    !value
}

fn glyph(character: char) -> Option<[u8; 7]> {
    let pattern = match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
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
        _ => return None,
    };
    Some(pattern)
}
