use crate::content::{
    ClipRegion, Color, DisplayCommand, LineCap, LineJoin, Matrix, Paint, PathSegment, Point,
    StrokeStyle, TextRun,
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
        fill_scratch: FillScratch::default(),
    };
    for command in commands {
        match command {
            DisplayCommand::Text(run) => canvas.draw_text(run)?,
            DisplayCommand::Figure { bbox, clips, .. } => {
                canvas.draw_figure_placeholder(*bbox, clips)?
            }
            DisplayCommand::Image {
                image,
                bbox,
                matrix,
                page_left,
                page_height,
                clips,
                ..
            } => {
                canvas.draw_image(image, *bbox, *matrix, *page_left, *page_height, clips)?;
            }
            DisplayCommand::Fill {
                path,
                paint,
                even_odd,
                clips,
                ..
            } => canvas.fill_path(path, *paint, *even_odd, clips)?,
            DisplayCommand::Stroke {
                path,
                paint,
                style,
                clips,
                ..
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

fn argb_paint(argb: u32) -> Paint {
    Paint {
        color: Color {
            red: ((argb >> 16) & 0xff) as u8,
            green: ((argb >> 8) & 0xff) as u8,
            blue: (argb & 0xff) as u8,
        },
        alpha: ((argb >> 24) & 0xff) as f32 / 255.0,
    }
}

fn transformed_glyph_contours(
    glyph: &GlyphOutline,
    origin_x: f32,
    baseline_y: f32,
    scale_x: f32,
    scale_y: f32,
) -> Vec<Vec<Point>> {
    glyph
        .contours
        .iter()
        .map(|contour| {
            contour
                .iter()
                .map(|point| Point {
                    x: origin_x + point.x * scale_x,
                    y: baseline_y - point.y * scale_y,
                })
                .collect()
        })
        .collect()
}

struct ScanEdge {
    start: Point,
    end: Point,
    min_y: f32,
    max_y: f32,
    ordinal: usize,
}

#[derive(Default)]
struct FillScratch {
    edges: Vec<ScanEdge>,
    active_edges: Vec<usize>,
    next_edge: usize,
    crossings: Vec<(f32, i32, usize)>,
    spans: Vec<(f32, f32)>,
    coverage: Vec<u8>,
}

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    fill_scratch: FillScratch,
}

impl Canvas {
    fn draw_image(
        &mut self,
        image: &docsight_core::DecodedImage,
        bbox: Rect,
        matrix: Matrix,
        page_left: f32,
        page_height: f32,
        clips: &[ClipRegion],
    ) -> Result<(), DocsightError> {
        let det = matrix.a * matrix.d - matrix.b * matrix.c;
        if det.abs() <= 1e-6 || !det.is_finite() {
            return Ok(());
        }
        let px0 = self.pixel_floor_x(bbox.x0).max(0);
        let py0 = self.pixel_floor_y(bbox.y0).max(0);
        let px1 = self.pixel_ceil_x(bbox.x1).min(self.width as i32);
        let py1 = self.pixel_ceil_y(bbox.y1).min(self.height as i32);
        if px0 >= px1 || py0 >= py1 {
            return Ok(());
        }
        for py in py0..py1 {
            for px in px0..px1 {
                let page_point = self.sample_point(px, py, 0.5, 0.5);
                if !clips.is_empty() && !clips.iter().all(|clip| clip_contains(clip, page_point)) {
                    continue;
                }
                let x_pdf = page_point.x + page_left;
                let y_pdf = page_height - page_point.y;
                let dx = x_pdf - matrix.e;
                let dy = y_pdf - matrix.f;
                let u = (matrix.d * dx - matrix.c * dy) / det;
                let v = (-matrix.b * dx + matrix.a * dy) / det;
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    continue;
                }
                let img_x =
                    ((u * image.width as f32).floor() as u32).min(image.width.saturating_sub(1));
                let img_y = (((1.0 - v) * image.height as f32).floor() as u32)
                    .min(image.height.saturating_sub(1));
                let Some(sample) = image.pixel(img_x, img_y) else {
                    continue;
                };
                let alpha = u32::from(sample[3]);
                if alpha == 0 {
                    continue;
                }
                let index = (py as usize * self.width as usize + px as usize) * 3;
                if let Some(slot) = self.pixels.get_mut(index..index + 3) {
                    for channel in 0..3 {
                        let source = u32::from(sample[channel]);
                        let destination = u32::from(slot[channel]);
                        slot[channel] =
                            ((source * alpha + destination * (255 - alpha)) / 255) as u8;
                    }
                }
            }
        }
        Ok(())
    }

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
        if matches!(run.render_mode, 3 | 7) {
            return Ok(());
        }
        let fill = matches!(run.render_mode, 0 | 2 | 4 | 6);
        let stroke = matches!(run.render_mode, 1 | 2 | 5 | 6);
        let fill_paint = argb_paint(run.argb);
        let stroke_paint = argb_paint(run.stroke_argb);
        if !run.glyphs.is_empty() {
            return self.draw_outline_text(run, fill, stroke, fill_paint, stroke_paint);
        }
        let character_count = run.text.chars().count();
        if character_count == 0 {
            return Ok(());
        }
        let advance = run.bbox.width() / character_count as f32;
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
            let visual_index = if run.mirrored_x {
                character_count - index - 1
            } else {
                index
            };
            let left = run.bbox.x0 + visual_index as f32 * advance;
            let cell_width = advance / 6.0;
            let cell_height = run.bbox.height() / 7.0;
            for (row, bits) in pattern.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    let visual_column = if run.mirrored_x { 4 - column } else { column };
                    let x0 = left + visual_column as f32 * cell_width;
                    let y0 = run.bbox.y0 + row as f32 * cell_height;
                    let x1 = x0 + cell_width * if run.bold { 1.35 } else { 1.0 };
                    let y1 = y0 + cell_height;
                    if stroke {
                        let expansion = run.stroke_style.width / 2.0;
                        self.fill_rect(
                            x0 - expansion,
                            y0 - expansion,
                            x1 + expansion,
                            y1 + expansion,
                            stroke_paint,
                            &run.clips,
                        );
                    }
                    if fill {
                        self.fill_rect(x0, y0, x1, y1, fill_paint, &run.clips);
                    }
                }
            }
        }
        Ok(())
    }

    fn draw_outline_text(
        &mut self,
        run: &TextRun,
        fill: bool,
        stroke: bool,
        fill_paint: Paint,
        stroke_paint: Paint,
    ) -> Result<(), DocsightError> {
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
        let direction = if run.mirrored_x { -1.0 } else { 1.0 };
        let mut cursor = if run.mirrored_x {
            run.bbox.x1
        } else {
            run.bbox.x0
        };
        for glyph in &run.glyphs {
            let scale_x = nominal_scale * horizontal_scale * direction;
            let contours =
                transformed_glyph_contours(glyph, cursor, run.baseline_y, scale_x, nominal_scale);
            if stroke {
                for contour in &contours {
                    if contour.is_empty() {
                        continue;
                    }
                    let mut path = Vec::with_capacity(contour.len() + 1);
                    path.push(PathSegment::Move(contour[0]));
                    path.extend(contour.iter().skip(1).copied().map(PathSegment::Line));
                    path.push(PathSegment::Close);
                    self.stroke_path(&path, stroke_paint, &run.stroke_style, &run.clips)?;
                }
            }
            if fill {
                self.fill_polygons(&contours, fill_paint, false, &run.clips);
            }
            cursor += glyph.advance * nominal_scale * horizontal_scale * direction;
        }
        Ok(())
    }

    fn fill_path(
        &mut self,
        path: &[PathSegment],
        paint: Paint,
        even_odd: bool,
        clips: &[ClipRegion],
    ) -> Result<(), DocsightError> {
        let polygons = flatten_subpaths(path, 0.125 / self.scale)?
            .into_iter()
            .filter(|polygon| polygon.len() >= 3)
            .collect::<Vec<_>>();
        if polygons.is_empty() {
            return Ok(());
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
        let coverage_len = raster_width.saturating_mul(raster_height);
        self.fill_scratch.coverage.clear();
        self.fill_scratch.coverage.resize(coverage_len, 0);
        self.fill_scratch.edges.clear();
        self.fill_scratch.edges.clear();
        for (ordinal, edge) in polygons
            .iter()
            .flat_map(|polygon| polygon.windows(2))
            .enumerate()
        {
            self.fill_scratch.edges.push(ScanEdge {
                start: edge[0],
                end: edge[1],
                min_y: edge[0].y.min(edge[1].y),
                max_y: edge[0].y.max(edge[1].y),
                ordinal,
            });
        }
        self.fill_scratch.edges.sort_by(|left, right| {
            left.min_y
                .total_cmp(&right.min_y)
                .then_with(|| left.ordinal.cmp(&right.ordinal))
        });
        self.fill_scratch.active_edges.clear();
        self.fill_scratch.next_edge = 0;
        let scale = self.scale;
        let offset_x = self.offset_x;
        let offset_y = self.offset_y;
        {
            let FillScratch {
                edges,
                active_edges,
                next_edge,
                crossings,
                spans,
                coverage,
            } = &mut self.fill_scratch;
            for y in y0..y1 {
                for sample_y in [0.125, 0.375, 0.625, 0.875] {
                    let scan_y = (y as f32 + sample_y) / scale + offset_y;
                    while *next_edge < edges.len()
                        && (edges[*next_edge].min_y.is_nan() || edges[*next_edge].min_y <= scan_y)
                    {
                        if !edges[*next_edge].min_y.is_nan() {
                            active_edges.push(*next_edge);
                        }
                        *next_edge += 1;
                    }
                    active_edges.retain(|index| edges[*index].max_y > scan_y);
                    scanline_spans_into(edges, active_edges, scan_y, even_odd, crossings, spans);
                    if spans.is_empty() {
                        continue;
                    }
                    let mut span_index = 0usize;
                    for x in x0..x1 {
                        for sample_x in [0.125, 0.375, 0.625, 0.875] {
                            let point = sample_point_values(
                                x, y, sample_x, sample_y, scale, offset_x, offset_y,
                            );
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
        }
        let width = self.width;
        let height = self.height;
        let pixels = &mut self.pixels;
        for (local_y, row) in self
            .fill_scratch
            .coverage
            .chunks_exact(raster_width)
            .enumerate()
        {
            for (local_x, count) in row.iter().enumerate() {
                if *count != 0 {
                    blend_pixel_at(
                        pixels,
                        width,
                        height,
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
        let half_width = self.stroke_half_width(style);
        let Some(area) = self.stroke_area(pieces, half_width, style) else {
            return;
        };
        let masks = self.stroke_masks(pieces, &area, half_width, style);
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let mask = masks[area.index(x, y)];
                if mask == 0 {
                    continue;
                }
                let covered = COVERAGE_SAMPLES
                    .iter()
                    .enumerate()
                    .filter(|(bit, (sample_x, sample_y))| {
                        mask & (1 << bit) != 0
                            && clips.iter().all(|clip| {
                                clip_contains(clip, self.sample_point(x, y, *sample_x, *sample_y))
                            })
                    })
                    .count();
                let coverage = covered as f32 / COVERAGE_SAMPLES.len() as f32;
                if coverage > 0.0 {
                    self.blend_pixel(x, y, paint.color, coverage * paint.alpha);
                }
            }
        }
    }

    fn stroke_half_width(&self, style: &StrokeStyle) -> f32 {
        if style.width == 0.0 {
            0.5 / self.scale
        } else {
            style.width / 2.0
        }
    }

    /// Pixels a stroke may touch: the bounds of all its points, expanded by the widest join.
    fn stroke_area(
        &self,
        pieces: &[StrokePiece],
        half_width: f32,
        style: &StrokeStyle,
    ) -> Option<PixelArea> {
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
        let area = PixelArea {
            x0: self.pixel_floor_x(min_x).max(0),
            y0: self.pixel_floor_y(min_y).max(0),
            x1: self.pixel_ceil_x(max_x).min(self.width as i32),
            y1: self.pixel_ceil_y(max_y).min(self.height as i32),
        };
        (area.x1 > area.x0 && area.y1 > area.y0).then_some(area)
    }

    /// Coverage-sample bitmask of every pixel in `area`, testing each segment, join and cap
    /// only over the pixels it can reach instead of over the whole stroke.
    fn stroke_masks(
        &self,
        pieces: &[StrokePiece],
        area: &PixelArea,
        half_width: f32,
        style: &StrokeStyle,
    ) -> Vec<u16> {
        let mut masks = vec![0u16; area.len()];
        let margin = 1.0 / self.scale;
        let strip_reach = half_width + margin;
        let vertex_reach = half_width * style.miter_limit.max(1.5) + margin;
        for piece in pieces {
            for pair in piece.points.windows(2) {
                let bounds = PointBounds::around(&[pair[0], pair[1]], strip_reach);
                self.mark_samples(&mut masks, area, bounds, |point| {
                    point_in_segment_strip(point, pair[0], pair[1], half_width)
                });
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
                    let bounds = PointBounds::around(&[current], vertex_reach);
                    self.mark_samples(&mut masks, area, bounds, |point| {
                        point_in_join(point, previous, current, next, half_width, style)
                    });
                }
            } else {
                for window in vertices.windows(3) {
                    let bounds = PointBounds::around(&[window[1]], vertex_reach);
                    self.mark_samples(&mut masks, area, bounds, |point| {
                        point_in_join(point, window[0], window[1], window[2], half_width, style)
                    });
                }
                if vertices.len() >= 2 {
                    let last = vertices.len() - 1;
                    for (endpoint, adjacent) in [
                        (vertices[0], vertices[1]),
                        (vertices[last], vertices[last - 1]),
                    ] {
                        let bounds = PointBounds::around(&[endpoint], vertex_reach);
                        self.mark_samples(&mut masks, area, bounds, |point| {
                            point_in_cap(point, endpoint, adjacent, half_width, style.cap)
                        });
                    }
                }
            }
        }
        masks
    }

    /// Sets the bit of every coverage sample inside `area` and `bounds` that `contains` accepts.
    fn mark_samples(
        &self,
        masks: &mut [u16],
        area: &PixelArea,
        bounds: PointBounds,
        contains: impl Fn(Point) -> bool,
    ) {
        let x0 = self.pixel_floor_x(bounds.min_x).max(area.x0);
        let y0 = self.pixel_floor_y(bounds.min_y).max(area.y0);
        let x1 = self.pixel_ceil_x(bounds.max_x).min(area.x1);
        let y1 = self.pixel_ceil_y(bounds.max_y).min(area.y1);
        for y in y0..y1 {
            for x in x0..x1 {
                let index = area.index(x, y);
                let mut mask = masks[index];
                if mask == u16::MAX {
                    continue;
                }
                for (bit, (sample_x, sample_y)) in COVERAGE_SAMPLES.iter().enumerate() {
                    if mask & (1 << bit) == 0
                        && contains(self.sample_point(x, y, *sample_x, *sample_y))
                    {
                        mask |= 1 << bit;
                    }
                }
                masks[index] = mask;
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
        sample_point_values(
            x,
            y,
            sample_x,
            sample_y,
            self.scale,
            self.offset_x,
            self.offset_y,
        )
    }

    fn blend_pixel(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        blend_pixel_at(
            &mut self.pixels,
            self.width,
            self.height,
            x,
            y,
            color,
            coverage,
        );
    }
}

fn sample_point_values(
    x: i32,
    y: i32,
    sample_x: f32,
    sample_y: f32,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
) -> Point {
    Point {
        x: (x as f32 + sample_x) / scale + offset_x,
        y: (y as f32 + sample_y) / scale + offset_y,
    }
}

fn blend_pixel_at(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    color: Color,
    coverage: f32,
) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = (y as usize * width as usize + x as usize) * 3;
    let coverage = coverage.clamp(0.0, 1.0);
    let inverse = 1.0 - coverage;
    pixels[index] =
        (f32::from(color.red) * coverage + f32::from(pixels[index]) * inverse).round() as u8;
    pixels[index + 1] =
        (f32::from(color.green) * coverage + f32::from(pixels[index + 1]) * inverse).round() as u8;
    pixels[index + 2] =
        (f32::from(color.blue) * coverage + f32::from(pixels[index + 2]) * inverse).round() as u8;
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

fn scanline_spans_into(
    edges: &[ScanEdge],
    active_edges: &[usize],
    y: f32,
    even_odd: bool,
    crossings: &mut Vec<(f32, i32, usize)>,
    spans: &mut Vec<(f32, f32)>,
) {
    crossings.clear();
    for &edge_index in active_edges {
        let edge = &edges[edge_index];
        let start = edge.start;
        let end = edge.end;
        let delta = if start.y <= y && end.y > y {
            1
        } else if start.y > y && end.y <= y {
            -1
        } else {
            continue;
        };
        let amount = (y - start.y) / (end.y - start.y);
        crossings.push((start.x + amount * (end.x - start.x), delta, edge.ordinal));
    }
    crossings.sort_by(|first, second| {
        first
            .0
            .total_cmp(&second.0)
            .then_with(|| first.2.cmp(&second.2))
    });
    spans.clear();
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
}

#[derive(Clone, Debug)]
struct StrokePiece {
    points: Vec<Point>,
    closed: bool,
}

/// Half-open pixel rectangle `[x0, x1) x [y0, y1)` of the raster.
struct PixelArea {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl PixelArea {
    fn len(&self) -> usize {
        (self.x1 - self.x0) as usize * (self.y1 - self.y0) as usize
    }

    fn index(&self, x: i32, y: i32) -> usize {
        (y - self.y0) as usize * (self.x1 - self.x0) as usize + (x - self.x0) as usize
    }
}

/// Axis-aligned bounds in page space that contain every point a stroke primitive can cover.
struct PointBounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl PointBounds {
    fn around(points: &[Point], reach: f32) -> Self {
        let mut bounds = Self {
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
        };
        for point in points {
            bounds.min_x = bounds.min_x.min(point.x - reach);
            bounds.min_y = bounds.min_y.min(point.y - reach);
            bounds.max_x = bounds.max_x.max(point.x + reach);
            bounds.max_y = bounds.max_y.max(point.y + reach);
        }
        bounds
    }
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

#[cfg(test)]
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
        '\u{fffd}' => [31, 17, 21, 17, 21, 17, 31],
        '\u{c6}' => [7, 12, 20, 23, 28, 20, 23],
        '\u{d0}' => [28, 18, 17, 31, 17, 18, 28],
        '\u{d8}' => [14, 19, 21, 21, 25, 17, 14],
        '\u{de}' => [16, 30, 17, 17, 30, 16, 16],
        '\u{df}' => [12, 18, 18, 28, 18, 18, 28],
        '\u{e6}' => [0, 0, 26, 5, 15, 20, 11],
        '\u{f0}' => [10, 4, 14, 1, 15, 17, 14],
        '\u{f7}' => [0, 4, 0, 31, 0, 4, 0],
        '\u{f8}' => [0, 0, 15, 19, 21, 25, 30],
        '\u{fe}' => [16, 16, 30, 17, 17, 30, 16],
        '\u{a2}' => [4, 14, 20, 20, 20, 14, 4],
        '\u{a3}' => [6, 9, 8, 28, 8, 8, 30],
        '\u{a4}' => [0, 17, 14, 10, 14, 17, 0],
        '\u{a5}' => [17, 17, 10, 31, 4, 31, 4],
        '\u{a6}' => [4, 4, 4, 0, 4, 4, 4],
        '\u{ab}' => [0, 5, 10, 20, 10, 5, 0],
        '\u{ac}' => [0, 0, 31, 1, 1, 0, 0],
        '\u{ae}' => [14, 17, 29, 21, 29, 17, 14],
        '\u{af}' => [31, 0, 0, 0, 0, 0, 0],
        '\u{b5}' => [0, 0, 17, 17, 17, 30, 16],
        '\u{b6}' => [15, 21, 21, 13, 5, 5, 5],
        '\u{b7}' => [0, 0, 0, 12, 12, 0, 0],
        '\u{bb}' => [0, 20, 10, 5, 10, 20, 0],
        '\u{bc}' => [8, 8, 4, 2, 9, 15, 1],
        '\u{bd}' => [8, 8, 4, 2, 11, 1, 3],
        '\u{be}' => [24, 8, 4, 2, 9, 15, 1],
        '\u{bf}' => [4, 0, 4, 8, 16, 17, 14],
        '\u{131}' => [0, 0, 12, 4, 4, 4, 14],
        '\u{152}' => [15, 20, 20, 23, 20, 20, 15],
        '\u{153}' => [0, 0, 26, 21, 23, 20, 11],
        '\u{160}' => [10, 4, 15, 16, 14, 1, 30],
        '\u{161}' => [10, 4, 15, 16, 14, 1, 30],
        '\u{178}' => [10, 0, 17, 10, 4, 4, 4],
        '\u{17d}' => [10, 4, 31, 2, 4, 16, 31],
        '\u{17e}' => [10, 4, 31, 2, 4, 8, 31],
        '\u{192}' => [3, 4, 4, 14, 4, 4, 24],
        '\u{3a9}' => [14, 17, 17, 17, 10, 10, 27],
        '\u{3c0}' => [0, 0, 31, 10, 10, 10, 19],
        '\u{201a}' => [0, 0, 0, 0, 4, 4, 8],
        '\u{201e}' => [0, 0, 0, 0, 10, 10, 20],
        '\u{2020}' => [4, 14, 4, 4, 4, 4, 0],
        '\u{2021}' => [4, 14, 4, 14, 4, 4, 0],
        '\u{2030}' => [17, 2, 4, 8, 21, 0, 5],
        '\u{2039}' => [0, 2, 4, 8, 4, 2, 0],
        '\u{203a}' => [0, 8, 4, 2, 4, 8, 0],
        '\u{2044}' => [1, 2, 2, 4, 8, 8, 16],
        '\u{20ac}' => [6, 9, 28, 8, 28, 9, 6],
        '\u{2122}' => [27, 21, 17, 0, 0, 0, 0],
        '\u{2202}' => [14, 1, 1, 15, 17, 17, 14],
        '\u{2206}' => [0, 4, 4, 10, 10, 17, 31],
        '\u{220f}' => [31, 10, 10, 10, 10, 10, 27],
        '\u{221a}' => [3, 2, 2, 18, 10, 4, 0],
        '\u{221e}' => [0, 0, 10, 21, 21, 10, 0],
        '\u{2248}' => [0, 13, 18, 0, 13, 18, 0],
        '\u{2260}' => [2, 2, 31, 4, 31, 8, 8],
        '\u{2264}' => [2, 4, 8, 4, 2, 0, 31],
        '\u{2265}' => [8, 4, 2, 4, 8, 0, 31],
        '\u{25ca}' => [4, 10, 17, 17, 17, 10, 4],
        '\u{fb01}' => [6, 9, 29, 8, 8, 8, 9],
        '\u{fb02}' => [6, 9, 29, 8, 8, 8, 11],
        '\u{2dd}' => [18, 9, 0, 0, 0, 0, 0],
        '\u{2d8}' => [17, 14, 0, 0, 0, 0, 0],
        '\u{2d9}' => [4, 0, 0, 0, 0, 0, 0],
        '\u{2da}' => [4, 10, 4, 0, 0, 0, 0],
        '\u{2db}' => [0, 0, 0, 0, 0, 4, 12],
        '\u{2c7}' => [10, 4, 0, 0, 0, 0, 0],
        '\u{2217}' => [0, 0, 21, 14, 21, 0, 0],
        '<' => [2, 4, 8, 16, 8, 4, 2],
        '>' => [8, 4, 2, 1, 2, 4, 8],
        '{' => [6, 8, 8, 16, 8, 8, 6],
        '}' => [24, 4, 4, 2, 4, 4, 24],
        '~' => [0, 0, 13, 18, 0, 0, 0],
        '\u{a1}' => [4, 0, 4, 4, 4, 4, 4],
        '×' => [0, 17, 10, 4, 10, 17, 0],
        '•' => [0, 0, 0, 0, 12, 12, 0],
        '…' => [0, 0, 0, 0, 0, 21, 21],
        '→' => [0, 4, 2, 31, 2, 4, 0],
        '─' => [0, 0, 0, 31, 0, 0, 0],
        '│' => [4, 4, 4, 4, 4, 4, 4],
        '└' => [4, 4, 4, 4, 4, 4, 31],
        '├' => [4, 4, 4, 4, 4, 4, 31],
        '\u{f0b7}' => [0, 0, 0, 0, 12, 12, 0],
        '´' => [4, 2, 0, 0, 0, 0, 0],
        '`' => [2, 4, 0, 0, 0, 0, 0],
        '¨' => [10, 0, 0, 0, 0, 0, 0],
        'ˆ' => [4, 10, 0, 0, 0, 0, 0],
        '˜' => [10, 0, 0, 0, 0, 0, 0],
        '¸' => [0, 0, 0, 0, 0, 4, 8],
        '^' => [4, 10, 17, 0, 0, 0, 0],
        '¹' => [4, 12, 4, 14, 0, 0, 0],
        '²' => [12, 2, 4, 14, 0, 0, 0],
        '³' => [12, 2, 4, 2, 12, 0, 0],
        '✓' => [0, 1, 2, 4, 20, 8, 0],
        'ª' => [14, 1, 15, 17, 15, 0, 31],
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
            "the PDF raster font cannot draw every character its encodings can produce: {missing:?}"
        );
    }

    use super::*;
    use crate::font::FontPoint;

    fn brute_force_masks(
        canvas: &Canvas,
        area: &PixelArea,
        pieces: &[StrokePiece],
        half_width: f32,
        style: &StrokeStyle,
    ) -> Vec<u16> {
        let mut masks = vec![0u16; area.len()];
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let mut mask = 0u16;
                for (bit, (sample_x, sample_y)) in COVERAGE_SAMPLES.iter().enumerate() {
                    let point = canvas.sample_point(x, y, *sample_x, *sample_y);
                    if pieces
                        .iter()
                        .any(|piece| stroke_contains(piece, point, half_width, style))
                    {
                        mask |= 1 << bit;
                    }
                }
                masks[area.index(x, y)] = mask;
            }
        }
        masks
    }

    #[test]
    fn culled_stroke_coverage_matches_testing_every_primitive_at_every_pixel() {
        let canvas = Canvas {
            width: 64,
            height: 64,
            pixels: vec![255; 64 * 64 * 3],
            scale: 1.75,
            offset_x: 3.3,
            offset_y: -2.1,
            fill_scratch: FillScratch::default(),
        };
        let point = |x: f32, y: f32| Point { x, y };
        let zigzag = vec![
            point(4.0, 5.0),
            point(30.0, 8.0),
            point(10.0, 20.0),
            point(40.0, 33.0),
            point(38.0, 34.5),
            point(39.0, 34.6),
            point(20.0, 38.0),
        ];
        let open = StrokePiece {
            points: zigzag.clone(),
            closed: false,
        };
        let closed = StrokePiece {
            points: vec![
                point(8.0, 22.0),
                point(30.0, 36.0),
                point(5.0, 38.0),
                point(8.0, 22.0),
            ],
            closed: true,
        };
        let piece_sets = [vec![open, closed], dashed_pieces(&zigzag, &[5.0, 3.0], 1.5)];
        let mut compared = 0;
        for width in [0.0, 1.0, 5.5] {
            for join in [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel] {
                for cap in [LineCap::Butt, LineCap::Round, LineCap::Square] {
                    for miter_limit in [1.0, 1.5, 10.0] {
                        let style = StrokeStyle {
                            width,
                            cap,
                            join,
                            miter_limit,
                            dash: Vec::new(),
                            dash_phase: 0.0,
                        };
                        let half_width = canvas.stroke_half_width(&style);
                        for pieces in &piece_sets {
                            let Some(area) = canvas.stroke_area(pieces, half_width, &style) else {
                                continue;
                            };
                            let culled = canvas.stroke_masks(pieces, &area, half_width, &style);
                            let reference =
                                brute_force_masks(&canvas, &area, pieces, half_width, &style);
                            assert!(
                                culled == reference,
                                "width {width}, join {join:?}, cap {cap:?}, miter limit {miter_limit}"
                            );
                            assert!(culled.iter().any(|mask| *mask != 0));
                            compared += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(compared, 162);
    }

    #[test]
    fn embedded_outline_uses_fractional_pixel_coverage() {
        let mut canvas = Canvas {
            width: 20,
            height: 20,
            pixels: vec![255; 20 * 20 * 3],
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            fill_scratch: FillScratch::default(),
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
        let contours = transformed_glyph_contours(&glyph, 2.0, 16.0, 1.0, 1.0);
        canvas.fill_polygons(
            &contours,
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
            fill_scratch: FillScratch::default(),
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
