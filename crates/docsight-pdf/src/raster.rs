use crate::content::{
    ClipRegion, Color, DisplayCommand, LineCap, LineJoin, Paint, PathSegment, Point, StrokeStyle,
    TextRun,
};
use crate::font::GlyphOutline;
use docsight_core::{DocsightError, Rect};

pub const MIN_DPI: u16 = 36;
pub const MAX_DPI: u16 = 600;
pub const MAX_RASTER_PIXELS: u64 = 25_000_000;
const MAX_FLATTENED_POINTS: usize = 1_000_000;
const COVERAGE_SAMPLES: [(f32, f32); 16] = [
    (0.125, 0.125),
    (0.375, 0.125),
    (0.625, 0.125),
    (0.875, 0.125),
    (0.125, 0.375),
    (0.375, 0.375),
    (0.625, 0.375),
    (0.875, 0.375),
    (0.125, 0.625),
    (0.375, 0.625),
    (0.625, 0.625),
    (0.875, 0.625),
    (0.125, 0.875),
    (0.375, 0.875),
    (0.625, 0.875),
    (0.875, 0.875),
];

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
            DisplayCommand::Figure { bbox, clips, .. } => {
                canvas.draw_figure_placeholder(*bbox, clips)?
            }
            DisplayCommand::Fill {
                path,
                paint,
                even_odd,
                clips,
            } => canvas.fill_path(path, *paint, *even_odd, clips)?,
            DisplayCommand::Stroke {
                path,
                paint,
                style,
                clips,
            } => {
                canvas.stroke_path(path, *paint, style, clips)?;
            }
        }
    }
    Ok(Raster {
        width,
        height,
        png: docsight_core::encode_png(width, height, &canvas.pixels)?,
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
    fn draw_figure_placeholder(
        &mut self,
        bbox: Rect,
        clips: &[ClipRegion],
    ) -> Result<(), DocsightError> {
        let color = Color {
            red: 128,
            green: 128,
            blue: 128,
        };
        let top_left = Point {
            x: bbox.x0,
            y: bbox.y0,
        };
        let top_right = Point {
            x: bbox.x1,
            y: bbox.y0,
        };
        let bottom_left = Point {
            x: bbox.x0,
            y: bbox.y1,
        };
        let bottom_right = Point {
            x: bbox.x1,
            y: bbox.y1,
        };
        for (start, end) in [
            (top_left, top_right),
            (top_right, bottom_right),
            (bottom_right, bottom_left),
            (bottom_left, top_left),
            (top_left, bottom_right),
            (top_right, bottom_left),
        ] {
            self.stroke_path(
                &[PathSegment::Move(start), PathSegment::Line(end)],
                Paint { color, alpha: 1.0 },
                &StrokeStyle {
                    width: 1.0,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    miter_limit: 10.0,
                    dash: Vec::new(),
                    dash_phase: 0.0,
                },
                clips,
            )?;
        }
        Ok(())
    }

    fn draw_text(&mut self, run: &TextRun) -> Result<(), DocsightError> {
        if !run.glyphs.is_empty() {
            let color = Color {
                red: ((run.argb >> 16) & 0xff) as u8,
                green: ((run.argb >> 8) & 0xff) as u8,
                blue: (run.argb & 0xff) as u8,
            };
            self.draw_outline_text(run, color, ((run.argb >> 24) & 0xff) as f32 / 255.0);
            return Ok(());
        }
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
        let alpha = ((run.argb >> 24) & 0xff) as f32 / 255.0;
        for (index, character) in run.text.chars().enumerate() {
            if character.is_whitespace() || character == '\u{200b}' {
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
                    self.fill_rect(x0, y0, x1, y1, Paint { color, alpha }, &run.clips);
                }
            }
        }
        Ok(())
    }

    fn draw_outline_text(&mut self, run: &TextRun, color: Color, alpha: f32) {
        let units_per_em = run
            .glyphs
            .first()
            .map(|glyph| glyph.units_per_em)
            .unwrap_or(1000.0);
        let nominal_scale = run.font_size / units_per_em;
        let advance = run
            .glyphs
            .iter()
            .map(|glyph| glyph.advance * nominal_scale)
            .sum::<f32>();
        let horizontal_scale = if advance > 0.0 {
            run.bbox.width() / advance
        } else {
            1.0
        };
        let mut cursor = run.bbox.x0;
        for glyph in &run.glyphs {
            self.fill_glyph(
                glyph,
                cursor,
                run.baseline_y,
                nominal_scale * horizontal_scale,
                nominal_scale,
                color,
                alpha,
                &run.clips,
            );
            cursor += glyph.advance * nominal_scale * horizontal_scale;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_glyph(
        &mut self,
        glyph: &GlyphOutline,
        origin_x: f32,
        baseline_y: f32,
        scale_x: f32,
        scale_y: f32,
        color: Color,
        alpha: f32,
        clips: &[ClipRegion],
    ) {
        if glyph.contours.is_empty() {
            return;
        }
        let transformed = glyph
            .contours
            .iter()
            .map(|contour| {
                contour
                    .iter()
                    .map(|point| Point {
                        x: origin_x + point.x * scale_x,
                        y: baseline_y - point.y * scale_y,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        self.fill_polygons(&transformed, Paint { color, alpha }, false, clips);
    }

    fn fill_path(
        &mut self,
        path: &[PathSegment],
        paint: Paint,
        even_odd: bool,
        clips: &[ClipRegion],
    ) -> Result<(), DocsightError> {
        let polygons = flatten_subpaths(path, 0.125 / self.scale)?;
        if polygons.is_empty() || polygons.iter().any(|polygon| polygon.len() < 3) {
            return Err(DocsightError::MalformedDocument {
                message: "filled PDF path has fewer than three points".to_owned(),
            });
        }
        let polygons = polygons
            .into_iter()
            .map(|mut polygon| {
                if polygon.first() != polygon.last()
                    && let Some(first) = polygon.first().copied()
                {
                    polygon.push(first);
                }
                polygon
            })
            .collect::<Vec<_>>();
        self.fill_polygons(&polygons, paint, even_odd, clips);
        Ok(())
    }

    fn stroke_path(
        &mut self,
        path: &[PathSegment],
        paint: Paint,
        style: &StrokeStyle,
        clips: &[ClipRegion],
    ) -> Result<(), DocsightError> {
        let subpaths = flatten_subpaths(path, 0.125 / self.scale)?;
        let mut pieces = Vec::new();
        for points in subpaths {
            pieces.extend(dashed_pieces(&points, &style.dash, style.dash_phase));
        }
        self.stroke_pieces(&pieces, paint, style, clips);
        Ok(())
    }

    fn fill_polygons(
        &mut self,
        polygons: &[Vec<Point>],
        paint: Paint,
        even_odd: bool,
        clips: &[ClipRegion],
    ) {
        let min_x = polygons
            .iter()
            .flatten()
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = polygons
            .iter()
            .flatten()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        let max_x = polygons
            .iter()
            .flatten()
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let max_y = polygons
            .iter()
            .flatten()
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let x0 = self.pixel_floor_x(min_x).max(0);
        let y0 = self.pixel_floor_y(min_y).max(0);
        let x1 = self.pixel_ceil_x(max_x).min(self.width as i32);
        let y1 = self.pixel_ceil_y(max_y).min(self.height as i32);
        let raster_width = (x1 - x0).max(0) as usize;
        let raster_height = (y1 - y0).max(0) as usize;
        if raster_width == 0 || raster_height == 0 {
            return;
        }
        let mut coverage = vec![0u8; raster_width.saturating_mul(raster_height)];
        for y in y0..y1 {
            for sample_y in [0.125, 0.375, 0.625, 0.875] {
                let scan_y = (y as f32 + sample_y) / self.scale + self.offset_y;
                let spans = scanline_spans(polygons, scan_y, even_odd);
                if spans.is_empty() {
                    continue;
                }
                let mut span_index = 0usize;
                for x in x0..x1 {
                    for sample_x in [0.125, 0.375, 0.625, 0.875] {
                        let point = self.sample_point(x, y, sample_x, sample_y);
                        while span_index < spans.len() && point.x >= spans[span_index].1 {
                            span_index += 1;
                        }
                        if span_index == spans.len() {
                            break;
                        }
                        if point.x >= spans[span_index].0
                            && clips.iter().all(|clip| clip_contains(clip, point))
                        {
                            let local_x = (x - x0) as usize;
                            let local_y = (y - y0) as usize;
                            let index = local_y * raster_width + local_x;
                            coverage[index] = coverage[index].saturating_add(1);
                        }
                    }
                }
            }
        }
        for (local_y, row) in coverage.chunks_exact(raster_width).enumerate() {
            for (local_x, count) in row.iter().enumerate() {
                if *count != 0 {
                    self.blend_pixel(
                        x0 + local_x as i32,
                        y0 + local_y as i32,
                        paint.color,
                        f32::from(*count) / COVERAGE_SAMPLES.len() as f32 * paint.alpha,
                    );
                }
            }
        }
    }

    fn stroke_pieces(
        &mut self,
        pieces: &[StrokePiece],
        paint: Paint,
        style: &StrokeStyle,
        clips: &[ClipRegion],
    ) {
        if pieces.is_empty() {
            return;
        }
        let half_width = if style.width == 0.0 {
            0.5 / self.scale
        } else {
            style.width / 2.0
        };
        let expansion = half_width * style.miter_limit.max(1.0) + 1.0 / self.scale;
        let min_x = pieces
            .iter()
            .flat_map(|piece| piece.points.iter())
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min)
            - expansion;
        let min_y = pieces
            .iter()
            .flat_map(|piece| piece.points.iter())
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min)
            - expansion;
        let max_x = pieces
            .iter()
            .flat_map(|piece| piece.points.iter())
            .map(|point| point.x)
            .fold(f32::NEG_INFINITY, f32::max)
            + expansion;
        let max_y = pieces
            .iter()
            .flat_map(|piece| piece.points.iter())
            .map(|point| point.y)
            .fold(f32::NEG_INFINITY, f32::max)
            + expansion;
        let x0 = self.pixel_floor_x(min_x).max(0);
        let y0 = self.pixel_floor_y(min_y).max(0);
        let x1 = self.pixel_ceil_x(max_x).min(self.width as i32);
        let y1 = self.pixel_ceil_y(max_y).min(self.height as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = COVERAGE_SAMPLES
                    .iter()
                    .filter(|(sample_x, sample_y)| {
                        let point = self.sample_point(x, y, *sample_x, *sample_y);
                        pieces
                            .iter()
                            .any(|piece| stroke_contains(piece, point, half_width, style))
                            && clips.iter().all(|clip| clip_contains(clip, point))
                    })
                    .count() as f32
                    / COVERAGE_SAMPLES.len() as f32;
                if coverage > 0.0 {
                    self.blend_pixel(x, y, paint.color, coverage * paint.alpha);
                }
            }
        }
    }

    fn fill_rect(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        paint: Paint,
        clips: &[ClipRegion],
    ) {
        let polygon = vec![
            Point { x: x0, y: y0 },
            Point { x: x1, y: y0 },
            Point { x: x1, y: y1 },
            Point { x: x0, y: y1 },
            Point { x: x0, y: y0 },
        ];
        self.fill_polygons(&[polygon], paint, false, clips);
    }

    fn pixel_floor_x(&self, value: f32) -> i32 {
        ((value - self.offset_x) * self.scale).floor() as i32
    }

    fn pixel_floor_y(&self, value: f32) -> i32 {
        ((value - self.offset_y) * self.scale).floor() as i32
    }

    fn pixel_ceil_x(&self, value: f32) -> i32 {
        ((value - self.offset_x) * self.scale).ceil() as i32
    }

    fn pixel_ceil_y(&self, value: f32) -> i32 {
        ((value - self.offset_y) * self.scale).ceil() as i32
    }

    fn sample_point(&self, x: i32, y: i32, sample_x: f32, sample_y: f32) -> Point {
        Point {
            x: (x as f32 + sample_x) / self.scale + self.offset_x,
            y: (y as f32 + sample_y) / self.scale + self.offset_y,
        }
    }

    fn blend_pixel(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 3;
        let coverage = coverage.clamp(0.0, 1.0);
        let inverse = 1.0 - coverage;
        self.pixels[index] = (f32::from(color.red) * coverage
            + f32::from(self.pixels[index]) * inverse)
            .round() as u8;
        self.pixels[index + 1] = (f32::from(color.green) * coverage
            + f32::from(self.pixels[index + 1]) * inverse)
            .round() as u8;
        self.pixels[index + 2] = (f32::from(color.blue) * coverage
            + f32::from(self.pixels[index + 2]) * inverse)
            .round() as u8;
    }
}

fn clip_contains(clip: &ClipRegion, point: Point) -> bool {
    if clip.even_odd {
        clip.polygons
            .iter()
            .filter(|polygon| winding_number(point, polygon) != 0)
            .count()
            % 2
            == 1
    } else {
        clip.polygons
            .iter()
            .map(|polygon| winding_number(point, polygon))
            .sum::<i32>()
            != 0
    }
}

fn scanline_spans(polygons: &[Vec<Point>], y: f32, even_odd: bool) -> Vec<(f32, f32)> {
    let mut crossings = Vec::new();
    for edge in polygons.iter().flat_map(|polygon| polygon.windows(2)) {
        let start = edge[0];
        let end = edge[1];
        let delta = if start.y <= y && end.y > y {
            1
        } else if start.y > y && end.y <= y {
            -1
        } else {
            continue;
        };
        let amount = (y - start.y) / (end.y - start.y);
        crossings.push((start.x + amount * (end.x - start.x), delta));
    }
    crossings.sort_by(|first, second| first.0.total_cmp(&second.0));
    let mut spans = Vec::new();
    let mut winding = 0i32;
    let mut parity = false;
    let mut previous = None;
    let mut cursor = 0usize;
    while cursor < crossings.len() {
        let x = crossings[cursor].0;
        let inside = if even_odd { parity } else { winding != 0 };
        if let Some(start) = previous
            && inside
            && start < x
        {
            spans.push((start, x));
        }
        let mut delta = 0i32;
        let mut count = 0usize;
        while cursor < crossings.len() && crossings[cursor].0 == x {
            delta += crossings[cursor].1;
            count += 1;
            cursor += 1;
        }
        winding -= delta;
        if !count.is_multiple_of(2) {
            parity = !parity;
        }
        previous = Some(x);
    }
    spans
}

#[derive(Clone, Debug)]
struct StrokePiece {
    points: Vec<Point>,
    closed: bool,
}

fn dashed_pieces(points: &[Point], pattern: &[f32], phase: f32) -> Vec<StrokePiece> {
    if points.len() < 2 {
        return Vec::new();
    }
    let closed = points.first() == points.last();
    if pattern.is_empty() {
        return vec![StrokePiece {
            points: points.to_vec(),
            closed,
        }];
    }
    let mut effective = pattern.to_vec();
    if effective.len() % 2 == 1 {
        effective.extend_from_slice(pattern);
    }
    let total = effective.iter().sum::<f32>();
    let mut offset = phase % total;
    let mut pattern_index = 0usize;
    for _ in 0..effective.len() {
        let length = effective[pattern_index];
        if length > 0.0 && offset < length {
            break;
        }
        if length > 0.0 {
            offset -= length;
        }
        pattern_index = (pattern_index + 1) % effective.len();
    }
    let mut remaining = effective[pattern_index] - offset;
    let mut on = pattern_index.is_multiple_of(2);
    let mut current = Vec::new();
    let mut result = Vec::new();
    for pair in points.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let length = distance(start, end);
        if length <= f32::EPSILON {
            continue;
        }
        let mut traversed = 0.0;
        while traversed < length {
            while remaining <= f32::EPSILON {
                if !on && current.len() >= 2 {
                    result.push(StrokePiece {
                        points: std::mem::take(&mut current),
                        closed: false,
                    });
                }
                pattern_index = (pattern_index + 1) % effective.len();
                on = pattern_index.is_multiple_of(2);
                remaining = effective[pattern_index];
            }
            let consumed = remaining.min(length - traversed);
            let segment_start = interpolate(start, end, traversed / length);
            let segment_end = interpolate(start, end, (traversed + consumed) / length);
            if on {
                if current.last().copied() != Some(segment_start) {
                    current.push(segment_start);
                }
                current.push(segment_end);
            } else if current.len() >= 2 {
                result.push(StrokePiece {
                    points: std::mem::take(&mut current),
                    closed: false,
                });
            }
            traversed += consumed;
            remaining -= consumed;
        }
    }
    if current.len() >= 2 {
        result.push(StrokePiece {
            points: current,
            closed: false,
        });
    }
    result
}

fn stroke_contains(
    piece: &StrokePiece,
    point: Point,
    half_width: f32,
    style: &StrokeStyle,
) -> bool {
    if piece
        .points
        .windows(2)
        .any(|pair| point_in_segment_strip(point, pair[0], pair[1], half_width))
    {
        return true;
    }
    let vertices = if piece.closed && piece.points.len() > 2 {
        &piece.points[..piece.points.len() - 1]
    } else {
        &piece.points[..]
    };
    if piece.closed {
        for index in 0..vertices.len() {
            let previous = vertices[(index + vertices.len() - 1) % vertices.len()];
            let current = vertices[index];
            let next = vertices[(index + 1) % vertices.len()];
            if point_in_join(point, previous, current, next, half_width, style) {
                return true;
            }
        }
    } else {
        for window in vertices.windows(3) {
            if point_in_join(point, window[0], window[1], window[2], half_width, style) {
                return true;
            }
        }
        if vertices.len() >= 2
            && (point_in_cap(point, vertices[0], vertices[1], half_width, style.cap)
                || point_in_cap(
                    point,
                    vertices[vertices.len() - 1],
                    vertices[vertices.len() - 2],
                    half_width,
                    style.cap,
                ))
        {
            return true;
        }
    }
    false
}

fn point_in_segment_strip(point: Point, start: Point, end: Point, half_width: f32) -> bool {
    let direction = subtract(end, start);
    let squared_length = dot(direction, direction);
    if squared_length <= f32::EPSILON {
        return false;
    }
    let relative = subtract(point, start);
    let projection = dot(relative, direction) / squared_length;
    if !(0.0..=1.0).contains(&projection) {
        return false;
    }
    cross(direction, relative).abs() / squared_length.sqrt() <= half_width
}

fn point_in_cap(
    point: Point,
    endpoint: Point,
    adjacent: Point,
    half_width: f32,
    cap: LineCap,
) -> bool {
    match cap {
        LineCap::Butt => false,
        LineCap::Round => distance(point, endpoint) <= half_width,
        LineCap::Square => {
            let direction = normalize(subtract(endpoint, adjacent));
            let relative = subtract(point, endpoint);
            let along = dot(relative, direction);
            let perpendicular = cross(direction, relative).abs();
            (0.0..=half_width).contains(&along) && perpendicular <= half_width
        }
    }
}

fn point_in_join(
    point: Point,
    previous: Point,
    current: Point,
    next: Point,
    half_width: f32,
    style: &StrokeStyle,
) -> bool {
    let incoming = normalize(subtract(current, previous));
    let outgoing = normalize(subtract(next, current));
    let turn = cross(incoming, outgoing);
    if turn.abs() <= 0.000_001 {
        return false;
    }
    if style.join == LineJoin::Round {
        return distance(point, current) <= half_width;
    }
    let side = -turn.signum();
    let outer_incoming = add(current, scale(left_normal(incoming), side * half_width));
    let outer_outgoing = add(current, scale(left_normal(outgoing), side * half_width));
    let mut polygon = vec![current, outer_incoming];
    if style.join == LineJoin::Miter {
        let denominator = cross(incoming, outgoing);
        let parameter = cross(subtract(outer_outgoing, outer_incoming), outgoing) / denominator;
        let miter = add(outer_incoming, scale(incoming, parameter));
        if distance(current, miter) <= half_width * style.miter_limit {
            polygon.push(miter);
        }
    }
    polygon.push(outer_outgoing);
    polygon.push(current);
    winding_number(point, &polygon) != 0
}

fn normalize(point: Point) -> Point {
    let length = (point.x * point.x + point.y * point.y).sqrt();
    if length <= f32::EPSILON {
        Point { x: 0.0, y: 0.0 }
    } else {
        Point {
            x: point.x / length,
            y: point.y / length,
        }
    }
}

fn left_normal(point: Point) -> Point {
    Point {
        x: -point.y,
        y: point.x,
    }
}

fn add(first: Point, second: Point) -> Point {
    Point {
        x: first.x + second.x,
        y: first.y + second.y,
    }
}

fn subtract(first: Point, second: Point) -> Point {
    Point {
        x: first.x - second.x,
        y: first.y - second.y,
    }
}

fn scale(point: Point, factor: f32) -> Point {
    Point {
        x: point.x * factor,
        y: point.y * factor,
    }
}

fn dot(first: Point, second: Point) -> f32 {
    first.x * second.x + first.y * second.y
}

fn cross(first: Point, second: Point) -> f32 {
    first.x * second.y - first.y * second.x
}

fn distance(first: Point, second: Point) -> f32 {
    let difference = subtract(first, second);
    dot(difference, difference).sqrt()
}

fn interpolate(start: Point, end: Point, amount: f32) -> Point {
    Point {
        x: start.x + (end.x - start.x) * amount,
        y: start.y + (end.y - start.y) * amount,
    }
}

fn flatten_subpaths(
    path: &[PathSegment],
    tolerance: f32,
) -> Result<Vec<Vec<Point>>, DocsightError> {
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
                flatten_cubic(
                    start,
                    *control1,
                    *control2,
                    *end,
                    tolerance,
                    0,
                    &mut current,
                )?;
                cursor = Some(*end);
            }
            PathSegment::Close => {
                if let Some(first) = current.first().copied()
                    && current.last().copied() != Some(first)
                {
                    current.push(first);
                }
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}

fn flatten_cubic(
    start: Point,
    first: Point,
    second: Point,
    end: Point,
    tolerance: f32,
    depth: u8,
    output: &mut Vec<Point>,
) -> Result<(), DocsightError> {
    if output.len() >= MAX_FLATTENED_POINTS {
        return Err(DocsightError::ResourceLimit {
            resource: "rasterized PDF curve points".to_owned(),
            limit: MAX_FLATTENED_POINTS as u64,
        });
    }
    if depth >= 16 || cubic_flatness(start, first, second, end) <= tolerance {
        output.push(end);
        return Ok(());
    }
    let start_first = midpoint(start, first);
    let first_second = midpoint(first, second);
    let second_end = midpoint(second, end);
    let left_second = midpoint(start_first, first_second);
    let right_first = midpoint(first_second, second_end);
    let middle = midpoint(left_second, right_first);
    flatten_cubic(
        start,
        start_first,
        left_second,
        middle,
        tolerance,
        depth + 1,
        output,
    )?;
    flatten_cubic(
        middle,
        right_first,
        second_end,
        end,
        tolerance,
        depth + 1,
        output,
    )
}

fn cubic_flatness(start: Point, first: Point, second: Point, end: Point) -> f32 {
    point_line_distance(first, start, end).max(point_line_distance(second, start, end))
}

fn point_line_distance(point: Point, start: Point, end: Point) -> f32 {
    let direction = subtract(end, start);
    let length = dot(direction, direction).sqrt();
    if length <= f32::EPSILON {
        distance(point, start)
    } else {
        cross(direction, subtract(point, start)).abs() / length
    }
}

fn midpoint(first: Point, second: Point) -> Point {
    Point {
        x: (first.x + second.x) / 2.0,
        y: (first.y + second.y) / 2.0,
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

fn glyph(character: char) -> Option<[u8; 7]> {
    let pattern = match character {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'É' => [2, 4, 31, 16, 30, 16, 31],
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
        'ö' => [10, 0, 14, 17, 17, 17, 14],
        'p' => [0, 0, 30, 17, 30, 16, 16],
        'q' => [0, 0, 15, 17, 15, 1, 1],
        'r' => [0, 0, 22, 25, 16, 16, 16],
        's' => [0, 0, 15, 16, 14, 1, 30],
        't' => [8, 8, 28, 8, 8, 9, 6],
        'u' => [0, 0, 17, 17, 17, 19, 13],
        'ú' => [4, 2, 17, 17, 17, 19, 13],
        'ü' => [10, 0, 17, 17, 17, 19, 13],
        'ő' => [10, 2, 14, 17, 17, 19, 13],
        'ű' => [10, 2, 17, 17, 17, 19, 13],
        'v' => [0, 0, 17, 17, 17, 10, 4],
        'w' => [0, 0, 17, 17, 21, 21, 10],
        'x' => [0, 0, 17, 10, 4, 10, 17],
        'y' => [0, 0, 17, 17, 15, 1, 14],
        'z' => [0, 0, 31, 2, 4, 8, 31],
        'Ö' => [10, 0, 14, 17, 17, 17, 14],
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
        '°' => [6, 9, 6, 0, 0, 0, 0],
        'º' => [6, 9, 9, 6, 0, 15, 0],
        '±' => [0, 4, 4, 31, 4, 4, 0],
        '−' | '–' | '—' => [0, 0, 0, 31, 0, 0, 0],
        '‘' | '’' | '\'' => [4, 4, 2, 0, 0, 0, 0],
        '“' | '”' | '"' => [10, 10, 5, 0, 0, 0, 0],
        'ϵ' => [0, 0, 14, 16, 30, 16, 14],
        '∑' => [31, 16, 8, 4, 2, 1, 31],
        '∫' => [6, 4, 4, 4, 4, 4, 12],
        '©' => [14, 17, 23, 21, 23, 17, 14],
        '§' => [14, 16, 12, 18, 5, 3, 14],
        '●' => [0, 14, 31, 31, 31, 14, 0],
        '□' => [31, 17, 17, 17, 17, 17, 31],
        '\u{f02a}' => [0, 14, 31, 31, 31, 14, 0],
        '$' => [4, 14, 20, 12, 5, 11, 4],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '*' => [0, 4, 21, 14, 21, 4, 0],
        '#' => [10, 31, 10, 10, 31, 10, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '&' => [12, 18, 20, 8, 21, 18, 13],
        '@' => [14, 17, 23, 21, 23, 16, 14],
        _ => return None,
    };
    Some(pattern)
}

pub fn glyph_coverage(text: &str) -> f32 {
    let mut total = 0_usize;
    let mut covered = 0_usize;
    for character in text.chars() {
        if character.is_whitespace() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontPoint;

    #[test]
    fn embedded_outline_uses_fractional_pixel_coverage() {
        let mut canvas = Canvas {
            width: 20,
            height: 20,
            pixels: vec![255; 20 * 20 * 3],
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        let glyph = GlyphOutline {
            contours: vec![vec![
                FontPoint { x: 1.0, y: 1.0 },
                FontPoint { x: 12.0, y: 1.0 },
                FontPoint { x: 1.0, y: 12.0 },
                FontPoint { x: 1.0, y: 1.0 },
            ]],
            advance: 1000.0,
            units_per_em: 1000.0,
        };
        canvas.fill_glyph(
            &glyph,
            2.0,
            16.0,
            1.0,
            1.0,
            Color {
                red: 0,
                green: 0,
                blue: 0,
            },
            1.0,
            &[],
        );
        assert!(canvas.pixels.chunks_exact(3).any(|pixel| {
            pixel[0] > 0 && pixel[0] < 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2]
        }));
    }

    #[test]
    fn vector_fill_uses_fractional_pixel_coverage() -> Result<(), DocsightError> {
        let mut canvas = Canvas {
            width: 20,
            height: 20,
            pixels: vec![255; 20 * 20 * 3],
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        canvas.fill_path(
            &[
                PathSegment::Move(Point { x: 1.0, y: 1.0 }),
                PathSegment::Line(Point { x: 12.0, y: 1.0 }),
                PathSegment::Line(Point { x: 1.0, y: 12.0 }),
                PathSegment::Close,
            ],
            Paint {
                color: Color {
                    red: 0,
                    green: 0,
                    blue: 0,
                },
                alpha: 1.0,
            },
            false,
            &[],
        )?;
        assert!(canvas.pixels.chunks_exact(3).any(|pixel| {
            pixel[0] > 0 && pixel[0] < 255 && pixel[0] == pixel[1] && pixel[1] == pixel[2]
        }));
        Ok(())
    }
}
