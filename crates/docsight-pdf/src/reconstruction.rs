use crate::content::{DisplayCommand, PathSegment, Point, TextRun};
use docsight_core::{
    Block, BlockContent, BlockKind, FigureBlock, HeadingBlock, ObjectId, ParagraphBlock, Rect,
    SourceSpan,
};
use docsight_tables::{RulingSegment, TextSpanItem, detect_tables};

pub(crate) struct ReconstructedPage {
    pub blocks: Vec<Block>,
    pub block_ids: Vec<ObjectId>,
}

pub(crate) fn reconstruct_page_semantics(
    page: u32,
    document_digest: &str,
    page_width: f32,
    page_height: f32,
    text_runs: &[TextRun],
    commands: &[DisplayCommand],
    global_reading_order: &mut u32,
) -> Result<ReconstructedPage, docsight_core::DocsightError> {
    let figures = commands
        .iter()
        .filter_map(|command| match command {
            DisplayCommand::Figure {
                bbox,
                resource_name,
                ..
            }
            | DisplayCommand::Image {
                bbox,
                resource_name,
                ..
            } => Some((*bbox, resource_name.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();

    if text_runs.is_empty() && figures.is_empty() {
        return Ok(ReconstructedPage {
            blocks: Vec::new(),
            block_ids: Vec::new(),
        });
    }

    let rulings = extract_rulings(commands);
    let span_items = merge_contiguous_runs_for_spans(text_runs);

    let tables = detect_tables(page, &span_items, &rulings, page_width, page_height);

    let mut covered_span_indices = std::collections::BTreeSet::new();
    for table in &tables {
        for (idx, span) in text_runs.iter().enumerate() {
            let cx = (span.bbox.x0 + span.bbox.x1) * 0.5;
            let cy = (span.bbox.y0 + span.bbox.y1) * 0.5;
            if cx >= table.bbox.x0 - 2.0
                && cx <= table.bbox.x1 + 2.0
                && cy >= table.bbox.y0 - 2.0
                && cy <= table.bbox.y1 + 2.0
            {
                covered_span_indices.insert(idx);
            }
        }
    }

    let remaining_runs: Vec<TextRun> = text_runs
        .iter()
        .enumerate()
        .filter(|(idx, _)| !covered_span_indices.contains(idx))
        .map(|(_, r)| r.clone())
        .collect();

    let lines = cluster_runs_into_lines(&remaining_runs);

    let mut font_sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    font_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_font_size = if !font_sizes.is_empty() {
        font_sizes[font_sizes.len() / 2]
    } else {
        12.0
    };

    let mut text_blocks: Vec<Block> = Vec::new();
    let mut current_paragraph: Option<(String, Rect, f32, u64, u64)> = None;

    let flush_paragraph = |current: &mut Option<(String, Rect, f32, u64, u64)>,
                           blocks: &mut Vec<Block>,
                           page_num: u32,
                           digest: &str| {
        if let Some((text, bbox, _, source_offset, source_end)) = current.take() {
            let anchor_length = source_end.saturating_sub(source_offset).max(1);
            let anchor_path =
                format!("pdf::page[{page_num}]::p[{source_offset}_{anchor_length}]::{text}");
            let p_id = ObjectId::new("p", digest, &anchor_path);
            blocks.push(Block {
                id: p_id,
                kind: BlockKind::Paragraph,
                page: Some(page_num),
                bbox: Some(bbox),
                z_index: 0,
                reading_order: 0,
                source: SourceSpan::with_range(
                    format!("pdf::page[{page_num}]::content"),
                    source_offset,
                    anchor_length,
                ),
                flags: docsight_core::LayoutFlags::default(),
                format: Default::default(),
                confidence: 0.85,
                continuations: Vec::new(),
                content: BlockContent::Paragraph(ParagraphBlock {
                    text,
                    style_id: None,
                }),
            });
        }
    };

    for line in &lines {
        let is_heading = if lines.len() <= 1 {
            false
        } else if line.font_size >= median_font_size * 1.18 {
            true
        } else {
            line.bold
                && line.font_size >= median_font_size
                && line.text.chars().count() < 80
                && !line.text.ends_with('.')
                && !line.text.ends_with(';')
        };

        if is_heading {
            flush_paragraph(
                &mut current_paragraph,
                &mut text_blocks,
                page,
                document_digest,
            );

            let level = if line.font_size >= median_font_size * 1.45 {
                1
            } else if line.font_size >= median_font_size * 1.25 {
                2
            } else if line.font_size >= median_font_size * 1.10 {
                3
            } else {
                4
            };

            let anchor_length = line.source_end.saturating_sub(line.source_offset).max(1);
            let h_id = ObjectId::new(
                "h",
                document_digest,
                &format!(
                    "pdf::page[{page}]::h[{}_{anchor_length}]::{}",
                    line.source_offset, line.text
                ),
            );

            text_blocks.push(Block {
                id: h_id,
                kind: BlockKind::Heading,
                page: Some(page),
                bbox: Some(line.bbox),
                z_index: 0,
                reading_order: 0,
                source: SourceSpan::with_range(
                    format!("pdf::page[{page}]::content"),
                    line.source_offset,
                    anchor_length,
                ),
                flags: docsight_core::LayoutFlags::default(),
                format: Default::default(),
                confidence: 0.90,
                continuations: Vec::new(),
                content: BlockContent::Heading(HeadingBlock {
                    level,
                    text: line.text.clone(),
                    style_id: None,
                }),
            });
        } else if let Some((
            ref mut p_text,
            ref mut p_bbox,
            ref mut last_y1,
            ref mut p_off,
            ref mut p_end,
        )) = current_paragraph
        {
            let gap = line.bbox.y0 - *last_y1;
            if gap <= line.font_size * 1.6 && gap >= -2.0 {
                p_text.push(' ');
                p_text.push_str(&line.text);
                let u_x0 = p_bbox.x0.min(line.bbox.x0);
                let u_y0 = p_bbox.y0.min(line.bbox.y0);
                let u_x1 = p_bbox.x1.max(line.bbox.x1);
                let u_y1 = p_bbox.y1.max(line.bbox.y1);
                if let Ok(rect) = Rect::new(u_x0, u_y0, u_x1, u_y1) {
                    *p_bbox = rect;
                }
                *last_y1 = line.bbox.y1;
                *p_off = (*p_off).min(line.source_offset);
                *p_end = (*p_end).max(line.source_end);
            } else {
                flush_paragraph(
                    &mut current_paragraph,
                    &mut text_blocks,
                    page,
                    document_digest,
                );
                current_paragraph = Some((
                    line.text.clone(),
                    line.bbox,
                    line.bbox.y1,
                    line.source_offset,
                    line.source_end,
                ));
            }
        } else {
            current_paragraph = Some((
                line.text.clone(),
                line.bbox,
                line.bbox.y1,
                line.source_offset,
                line.source_end,
            ));
        }
    }

    flush_paragraph(
        &mut current_paragraph,
        &mut text_blocks,
        page,
        document_digest,
    );

    let mut all_page_blocks: Vec<Block> = Vec::new();
    for (t_idx, table) in tables.into_iter().enumerate() {
        all_page_blocks.push(table.to_block(document_digest, t_idx, 0));
    }
    for (figure_index, (bbox, resource_name)) in figures.into_iter().enumerate() {
        let figure_id = ObjectId::new(
            "fig",
            document_digest,
            &format!("pdf::page[{page}]::xobject[{figure_index}]::{resource_name}"),
        );
        all_page_blocks.push(Block {
            id: figure_id,
            kind: BlockKind::Figure,
            page: Some(page),
            bbox: Some(bbox),
            z_index: 0,
            reading_order: 0,
            source: SourceSpan::new(format!("pdf::page[{page}]::xobject::{resource_name}")),
            flags: docsight_core::LayoutFlags::default(),
            format: Default::default(),
            confidence: 0.75,
            continuations: Vec::new(),
            content: BlockContent::Figure(FigureBlock {
                alt_text: None,
                caption: None,
                resource_id: Some(resource_name),
                width_pt: Some(bbox.width()),
                height_pt: Some(bbox.height()),
            }),
        });
    }
    all_page_blocks.extend(text_blocks);

    all_page_blocks.sort_by(|a, b| {
        let ay = a.bbox.map(|r| r.y0).unwrap_or(0.0);
        let by = b.bbox.map(|r| r.y0).unwrap_or(0.0);
        ay.partial_cmp(&by)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ax = a.bbox.map(|r| r.x0).unwrap_or(0.0);
                let bx = b.bbox.map(|r| r.x0).unwrap_or(0.0);
                ax.partial_cmp(&bx).unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut block_ids = Vec::new();
    for block in &mut all_page_blocks {
        *global_reading_order = global_reading_order.checked_add(1).ok_or_else(|| {
            docsight_core::DocsightError::ResourceLimit {
                resource: "reading order items".to_owned(),
                limit: u64::from(u32::MAX),
            }
        })?;
        block.reading_order = *global_reading_order;
        block_ids.push(block.id.clone());
    }

    Ok(ReconstructedPage {
        blocks: all_page_blocks,
        block_ids,
    })
}

#[derive(Clone, Debug)]
struct LineCandidate {
    text: String,
    bbox: Rect,
    font_size: f32,
    bold: bool,
    source_offset: u64,
    source_end: u64,
}

fn cluster_runs_into_lines(runs: &[TextRun]) -> Vec<LineCandidate> {
    if runs.is_empty() {
        return Vec::new();
    }

    let mut sorted_runs = runs.to_vec();
    sorted_runs.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut line_groups: Vec<Vec<TextRun>> = Vec::new();
    for run in sorted_runs {
        let run_cy = (run.bbox.y0 + run.bbox.y1) * 0.5;
        let mut placed = false;

        for group in &mut line_groups {
            if let Some(first) = group.first() {
                let group_cy = (first.bbox.y0 + first.bbox.y1) * 0.5;
                let overlap_h = first.bbox.height().min(run.bbox.height());
                if (run_cy - group_cy).abs() <= overlap_h * 0.45 {
                    group.push(run.clone());
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            line_groups.push(vec![run]);
        }
    }

    let mut lines = Vec::new();
    for mut group in line_groups {
        group.sort_by(|a, b| {
            a.bbox
                .x0
                .partial_cmp(&b.bbox.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut line_text = String::new();
        let mut prev_x1: Option<f32> = None;
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut total_font_size = 0.0;
        let mut bold_count = 0;
        let mut source_offset = u64::MAX;
        let mut source_end = 0_u64;

        for run in &group {
            if let Some(last_x) = prev_x1 {
                let gap = run.bbox.x0 - last_x;
                let space_threshold = (run.font_size * 0.22).max(2.2);
                if gap >= space_threshold
                    && !line_text.ends_with(char::is_whitespace)
                    && !run.text.starts_with(char::is_whitespace)
                {
                    line_text.push(' ');
                }
            }
            line_text.push_str(&run.text);
            prev_x1 = Some(run.bbox.x1);

            min_x = min_x.min(run.bbox.x0);
            min_y = min_y.min(run.bbox.y0);
            max_x = max_x.max(run.bbox.x1);
            max_y = max_y.max(run.bbox.y1);

            total_font_size += run.font_size;
            if run.bold {
                bold_count += 1;
            }
            source_offset = source_offset.min(run.source_offset);
            source_end = source_end.max(run.source_offset.saturating_add(run.source_length));
        }
        if source_offset == u64::MAX {
            source_offset = 0;
        }

        let bbox = match Rect::new(min_x, min_y, max_x, max_y) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let count = group.len() as f32;
        let font_size = if count > 0.0 {
            total_font_size / count
        } else {
            10.0
        };
        let bold = bold_count > group.len() / 2;

        lines.push(LineCandidate {
            text: line_text,
            bbox,
            font_size,
            source_offset,
            source_end,
            bold,
        });
    }

    lines.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    lines
}

pub(crate) fn extract_rulings(commands: &[DisplayCommand]) -> Vec<RulingSegment> {
    let mut rulings = Vec::new();
    for cmd in commands {
        match cmd {
            DisplayCommand::Stroke { path, .. } => {
                let mut curr: Option<Point> = None;
                let mut start: Option<Point> = None;
                for seg in path {
                    match seg {
                        PathSegment::Move(p) => {
                            curr = Some(*p);
                            start = Some(*p);
                        }
                        PathSegment::Line(p) => {
                            if let Some(prev) = curr {
                                rulings.push(RulingSegment {
                                    x0: prev.x,
                                    y0: prev.y,
                                    x1: p.x,
                                    y1: p.y,
                                });
                            }
                            curr = Some(*p);
                        }
                        PathSegment::Cubic(_, _, p) => {
                            curr = Some(*p);
                        }
                        PathSegment::Close => {
                            if let (Some(prev), Some(st)) = (curr, start) {
                                rulings.push(RulingSegment {
                                    x0: prev.x,
                                    y0: prev.y,
                                    x1: st.x,
                                    y1: st.y,
                                });
                            }
                            curr = start;
                        }
                    }
                }
            }
            DisplayCommand::Fill { path, .. } => {
                let mut points = Vec::new();
                let mut curved = false;
                for seg in path {
                    match seg {
                        PathSegment::Move(p) => {
                            if !curved {
                                append_filled_ruling(&points, &mut rulings);
                            }
                            curved = false;
                            points.clear();
                            points.push(*p);
                        }
                        PathSegment::Line(p) => points.push(*p),
                        PathSegment::Cubic(_, _, _) => curved = true,
                        PathSegment::Close => {
                            if !curved {
                                append_filled_ruling(&points, &mut rulings);
                            }
                            curved = false;
                            points.clear();
                        }
                    }
                }
                if !curved {
                    append_filled_ruling(&points, &mut rulings);
                }
            }
            _ => {}
        }
    }
    rulings
}

fn append_filled_ruling(points: &[Point], rulings: &mut Vec<RulingSegment>) {
    if points.len() < 4 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return;
    }
    let x0 = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let x1 = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let y0 = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let y1 = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    if !points
        .iter()
        .all(|p| (p.x == x0 || p.x == x1) && (p.y == y0 || p.y == y1))
    {
        return;
    }
    if points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .any(|(a, b)| a.x != b.x && a.y != b.y)
        || [(x0, y0), (x0, y1), (x1, y0), (x1, y1)]
            .iter()
            .any(|(x, y)| !points.iter().any(|point| point.x == *x && point.y == *y))
    {
        return;
    }
    if y1 > y0 && y1 - y0 <= 3.5 && x1 - x0 >= 6.0 {
        rulings.push(RulingSegment {
            x0,
            y0: (y0 + y1) * 0.5,
            x1,
            y1: (y0 + y1) * 0.5,
        });
    } else if x1 > x0 && x1 - x0 <= 3.5 && y1 - y0 >= 6.0 {
        rulings.push(RulingSegment {
            x0: (x0 + x1) * 0.5,
            y0,
            x1: (x0 + x1) * 0.5,
            y1,
        });
    }
}

fn merge_contiguous_runs_for_spans(runs: &[TextRun]) -> Vec<TextSpanItem> {
    if runs.is_empty() {
        return Vec::new();
    }
    let mut sorted = runs.to_vec();
    sorted.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut items: Vec<TextSpanItem> = Vec::new();
    for run in sorted {
        if let Some(last) = items.last_mut() {
            let same_line = (run.bbox.y0 - last.bbox.y0).abs()
                <= (last.font_size.min(run.font_size) * 0.35).max(3.0);
            let gap = run.bbox.x0 - last.bbox.x1;
            let intra_gap_limit = (last.font_size.max(run.font_size) * 0.22).max(2.2);
            let min_negative_gap = -last.font_size.max(run.font_size) * 0.3;
            let is_intra_word = same_line
                && last.bold == run.bold
                && gap <= intra_gap_limit
                && gap >= min_negative_gap
                && !last.text.ends_with(char::is_whitespace)
                && !run.text.starts_with(char::is_whitespace);
            if is_intra_word {
                last.text.push_str(&run.text);
                last.bbox = match Rect::new(
                    last.bbox.x0.min(run.bbox.x0),
                    last.bbox.y0.min(run.bbox.y0),
                    last.bbox.x1.max(run.bbox.x1),
                    last.bbox.y1.max(run.bbox.y1),
                ) {
                    Ok(r) => r,
                    Err(_) => last.bbox,
                };
                continue;
            }
        }
        items.push(TextSpanItem {
            text: run.text.clone(),
            bbox: run.bbox,
            font_size: run.font_size,
            bold: run.bold,
        });
    }
    items
}

#[cfg(test)]
mod ruling_tests {
    use super::*;

    #[test]
    fn accepts_thin_rectangles_but_rejects_diagonals() {
        let rectangle = [
            Point { x: 0.0, y: 0.0 },
            Point { x: 6.0, y: 0.0 },
            Point { x: 6.0, y: 1.0 },
            Point { x: 0.0, y: 1.0 },
        ];
        let mut rulings = Vec::new();
        append_filled_ruling(&rectangle, &mut rulings);
        assert_eq!(rulings.len(), 1);
        append_filled_ruling(
            &[rectangle[0], rectangle[2], rectangle[1], rectangle[3]],
            &mut rulings,
        );
        assert_eq!(rulings.len(), 1);
        append_filled_ruling(
            &[rectangle[0], rectangle[1], rectangle[2], rectangle[0]],
            &mut rulings,
        );
        assert_eq!(rulings.len(), 1);
    }
}
