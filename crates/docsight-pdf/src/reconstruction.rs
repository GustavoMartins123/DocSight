use crate::content::{DisplayCommand, PathSegment, Point, TextRun};
use docsight_core::{
    Block, BlockContent, BlockKind, HeadingBlock, ObjectId, ParagraphBlock, Rect, SourceSpan,
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
    if text_runs.is_empty() {
        return Ok(ReconstructedPage {
            blocks: Vec::new(),
            block_ids: Vec::new(),
        });
    }

    let rulings = extract_rulings(commands);
    let span_items: Vec<TextSpanItem> = text_runs
        .iter()
        .map(|run| TextSpanItem {
            text: run.text.clone(),
            bbox: run.bbox,
            font_size: run.font_size,
            bold: run.bold,
        })
        .collect();

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
    let mut current_paragraph: Option<(String, Rect, f32)> = None;

    let flush_paragraph = |current: &mut Option<(String, Rect, f32)>,
                           blocks: &mut Vec<Block>,
                           page_num: u32,
                           digest: &str| {
        if let Some((text, bbox, _)) = current.take() {
            let p_id = ObjectId::new(
                "p",
                digest,
                &format!(
                    "pdf::page[{page_num}]::p[{:.0}_{:.0}]::{text}",
                    bbox.x0, bbox.y0
                ),
            );
            blocks.push(Block {
                id: p_id,
                kind: BlockKind::Paragraph,
                page: Some(page_num),
                bbox: Some(bbox),
                z_index: 0,
                reading_order: 0,
                source: SourceSpan::new(format!(
                    "pdf::page[{page_num}]::p[{:.0}_{:.0}]",
                    bbox.x0, bbox.y0
                )),
                confidence: 0.85,
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

            let h_id = ObjectId::new(
                "h",
                document_digest,
                &format!(
                    "pdf::page[{page}]::h[{:.0}_{:.0}]::{}",
                    line.bbox.x0, line.bbox.y0, line.text
                ),
            );

            text_blocks.push(Block {
                id: h_id,
                kind: BlockKind::Heading,
                page: Some(page),
                bbox: Some(line.bbox),
                z_index: 0,
                reading_order: 0,
                source: SourceSpan::new(format!(
                    "pdf::page[{page}]::h[{:.0}_{:.0}]",
                    line.bbox.x0, line.bbox.y0
                )),
                confidence: 0.90,
                content: BlockContent::Heading(HeadingBlock {
                    level,
                    text: line.text.clone(),
                    style_id: None,
                }),
            });
        } else if let Some((ref mut p_text, ref mut p_bbox, ref mut last_y1)) = current_paragraph {
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
            } else {
                flush_paragraph(
                    &mut current_paragraph,
                    &mut text_blocks,
                    page,
                    document_digest,
                );
                current_paragraph = Some((line.text.clone(), line.bbox, line.bbox.y1));
            }
        } else {
            current_paragraph = Some((line.text.clone(), line.bbox, line.bbox.y1));
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

        for run in &group {
            if let Some(last_x) = prev_x1 {
                let gap = run.bbox.x0 - last_x;
                if gap >= 2.0 {
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
                let mut min_x = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut min_y = f32::INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                let mut count = 0;
                for seg in path {
                    let pt = match seg {
                        PathSegment::Move(p) | PathSegment::Line(p) => Some(*p),
                        PathSegment::Cubic(_, _, p) => Some(*p),
                        PathSegment::Close => None,
                    };
                    if let Some(p) = pt {
                        min_x = min_x.min(p.x);
                        max_x = max_x.max(p.x);
                        min_y = min_y.min(p.y);
                        max_y = max_y.max(p.y);
                        count += 1;
                    }
                }
                if count >= 3 {
                    let w = (max_x - min_x).abs();
                    let h = (max_y - min_y).abs();
                    if h <= 3.5 && w >= 15.0 {
                        rulings.push(RulingSegment {
                            x0: min_x,
                            y0: (min_y + max_y) * 0.5,
                            x1: max_x,
                            y1: (min_y + max_y) * 0.5,
                        });
                    } else if w <= 3.5 && h >= 15.0 {
                        rulings.push(RulingSegment {
                            x0: (min_x + max_x) * 0.5,
                            y0: min_y,
                            x1: (min_x + max_x) * 0.5,
                            y1: max_y,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    rulings
}
