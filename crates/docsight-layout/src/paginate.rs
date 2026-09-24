use crate::geometry::{SectionGeometry, SectionRange};
use crate::layout::MAX_LAYOUT_PAGES;
use crate::measure::{Measured, TextMetrics};
use docsight_core::{
    BlockKind, Diagnostic, DiagnosticSeverity, DocsightError, Document, ObjectId, SectionStart,
};
use std::collections::BTreeSet;
use std::ops::Range;

const LINE_FIT_TOLERANCE: f32 = 0.001;
const MINIMUM_SPLIT_LINES: usize = 2;
const KEEP_CHAIN_FAST_GUARD: f32 = 0.01;

#[derive(Clone, Debug)]
pub(crate) struct PageRecord {
    pub number: u32,
    pub display_number: u32,
    pub section: usize,
    pub sections: BTreeSet<usize>,
    pub geometry: SectionGeometry,
    pub first_of_section: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Fragment {
    pub page: u32,
    pub top: f32,
    pub lines: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Placement {
    pub section: usize,
    pub fragments: Vec<Fragment>,
}

pub(crate) struct Pagination {
    pub pages: Vec<PageRecord>,
    pub placements: Vec<Placement>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Clone)]
struct KeepChainInfo {
    end: usize,
    fast_height: f32,
    follower_page_break: bool,
}

struct Paginator<'a> {
    document: &'a Document,
    geometries: &'a [SectionGeometry],
    measured: &'a [Measured],
    keep_chains: Vec<KeepChainInfo>,
    pages: Vec<PageRecord>,
    placements: Vec<Placement>,
    warnings: Vec<Diagnostic>,
    cursor: f32,
    top: f32,
    bottom: f32,
    has_content: bool,
    pending_break: bool,
}

pub(crate) fn paginate(
    document: &Document,
    ranges: &[SectionRange],
    geometries: &[SectionGeometry],
    measured: &[Measured],
) -> Result<Pagination, DocsightError> {
    let keep_chains = build_keep_chains(document, measured, ranges);
    let mut paginator = Paginator {
        document,
        geometries,
        measured,
        keep_chains,
        pages: Vec::new(),
        placements: Vec::with_capacity(measured.len()),
        warnings: Vec::new(),
        cursor: 0.0,
        top: 0.0,
        bottom: 0.0,
        has_content: false,
        pending_break: false,
    };
    let last_range = ranges.len().saturating_sub(1);
    for (position, range) in ranges.iter().enumerate() {
        if range.blocks.is_empty() && !(position == last_range && paginator.pages.is_empty()) {
            continue;
        }
        paginator.start_section(range.section)?;
        for index in range.blocks.clone() {
            paginator.place_block(index, range)?;
        }
    }
    Ok(Pagination {
        pages: paginator.pages,
        placements: paginator.placements,
        warnings: paginator.warnings,
    })
}

fn keep_boundary_near(value: f32, boundary: f32) -> bool {
    let scale = value.abs().max(boundary.abs()).max(1.0);
    (value - boundary).abs() <= KEEP_CHAIN_FAST_GUARD * scale
}

fn build_keep_chains(
    document: &Document,
    measured: &[Measured],
    ranges: &[SectionRange],
) -> Vec<KeepChainInfo> {
    let mut infos = vec![
        KeepChainInfo {
            end: 0,
            fast_height: 0.0,
            follower_page_break: false,
        };
        measured.len()
    ];
    let mut chain_sum = vec![0.0_f64; measured.len()];
    for range in ranges {
        for index in (range.blocks.start..range.blocks.end).rev() {
            if !document.blocks[index].flags.keep_with_next {
                infos[index].end = index;
                continue;
            }
            let next = index.saturating_add(1);
            let next_is_keep =
                next < range.blocks.end && document.blocks[next].flags.keep_with_next;
            let end = if next_is_keep { infos[next].end } else { next };
            let successor = if next_is_keep { chain_sum[next] } else { 0.0 };
            let chain_total = measured[index].full_height() as f64 + successor;
            chain_sum[index] = chain_total;
            let follower_page_break =
                end < range.blocks.end && document.blocks[end].flags.page_break_before;
            let mut total = chain_total;
            if !follower_page_break && end < range.blocks.end {
                total += match &measured[end] {
                    Measured::Text(metrics) => {
                        metrics.leading_height(if document.blocks[end].flags.widow_control {
                            MINIMUM_SPLIT_LINES
                        } else {
                            1
                        }) as f64
                    }
                    Measured::Atomic { height } => *height as f64,
                };
            }
            infos[index] = KeepChainInfo {
                end,
                fast_height: total as f32,
                follower_page_break,
            };
        }
    }
    infos
}

impl Paginator<'_> {
    fn start_section(&mut self, section: usize) -> Result<(), DocsightError> {
        let geometry = self.geometries[section];
        let start = self.document.sections[section].start;
        let Some(current) = self.pages.last() else {
            return self.open_page(section, true);
        };
        let current_section = current.section;
        let same_page_size = current.geometry.same_page_size(&geometry);
        match start {
            SectionStart::Continuous if !self.pending_break && same_page_size => {
                self.pages
                    .last_mut()
                    .ok_or_else(|| DocsightError::BackendFailure {
                        backend: "docsight-layout".to_owned(),
                        message: "continuous section transition has no current page".to_owned(),
                    })?
                    .sections
                    .insert(section);
                self.cursor = self.cursor.max(geometry.margin_top);
                self.top = geometry.margin_top;
                self.bottom = geometry.content_bottom();
                Ok(())
            }
            SectionStart::EvenPage | SectionStart::OddPage => {
                let wants_even = start == SectionStart::EvenPage;
                let next_physical = u32::try_from(self.pages.len())
                    .ok()
                    .and_then(|count| count.checked_add(1))
                    .ok_or_else(page_limit)?;
                if next_physical.is_multiple_of(2) != wants_even {
                    self.open_page(current_section, false)?;
                }
                self.open_page(section, true)
            }
            SectionStart::NextColumn => {
                let page = current.number;
                let section_record = &self.document.sections[section];
                self.warnings.push(Diagnostic {
                    code: "DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED".to_owned(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "section {} starts at the next text column on page {page}",
                        section_record.section_index
                    ),
                    effect: "the section starts on a new page because multi-column flow is not supported"
                        .to_owned(),
                    object: Some(section_record.id.clone()),
                    page: Some(page),
                    occurrences: None,
                });
                self.open_page(section, true)
            }
            SectionStart::NextPage | SectionStart::Continuous => self.open_page(section, true),
        }
    }

    fn next_display_number(
        &self,
        section: usize,
        first_of_section: bool,
    ) -> Result<u32, DocsightError> {
        if first_of_section && let Some(start) = self.document.sections[section].page_number_start {
            return Ok(start);
        }
        match self.pages.last() {
            Some(previous) => previous
                .display_number
                .checked_add(1)
                .ok_or_else(page_limit),
            None => Ok(1),
        }
    }

    fn open_page(&mut self, section: usize, first_of_section: bool) -> Result<(), DocsightError> {
        let number = u32::try_from(self.pages.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .filter(|number| *number <= MAX_LAYOUT_PAGES)
            .ok_or_else(page_limit)?;
        let geometry = self.geometries[section];
        let display_number = self.next_display_number(section, first_of_section)?;
        self.pages.push(PageRecord {
            number,
            display_number,
            section,
            sections: BTreeSet::from([section]),
            geometry,
            first_of_section,
        });
        self.cursor = geometry.margin_top;
        self.top = geometry.margin_top;
        self.bottom = geometry.content_bottom();
        self.has_content = false;
        self.pending_break = false;
        Ok(())
    }

    fn continue_on_new_page(&mut self, section: usize) -> Result<(), DocsightError> {
        self.open_page(section, false)
    }

    fn current_page(&self) -> Result<u32, DocsightError> {
        self.pages
            .last()
            .map(|page| page.number)
            .ok_or_else(|| DocsightError::BackendFailure {
                backend: "docsight-layout".to_owned(),
                message: "pagination placed content before opening a page".to_owned(),
            })
    }

    fn remaining(&self) -> f32 {
        self.bottom - self.cursor
    }

    fn content_height(&self) -> f32 {
        self.bottom - self.top
    }

    fn place_block(&mut self, index: usize, range: &SectionRange) -> Result<(), DocsightError> {
        let document = self.document;
        let measured = self.measured;
        let block = &document.blocks[index];
        let flags = block.flags;
        if self.pending_break || (flags.page_break_before && self.has_content) {
            self.continue_on_new_page(range.section)?;
        }
        if flags.keep_with_next {
            self.apply_keep_chain(index, range)?;
        }
        let fragments = match &measured[index] {
            Measured::Text(metrics) => self.place_text(
                metrics,
                flags.keep_lines,
                flags.widow_control,
                &block.id,
                range,
            )?,
            Measured::Atomic { height } => {
                self.place_atomic(*height, block.kind, &block.id, range)?
            }
        };
        self.placements.push(Placement {
            section: range.section,
            fragments,
        });
        if flags.break_after {
            self.pending_break = true;
        }
        Ok(())
    }

    fn apply_keep_chain(
        &mut self,
        index: usize,
        range: &SectionRange,
    ) -> Result<(), DocsightError> {
        let info = &self.keep_chains[index];
        if info.follower_page_break {
            return Ok(());
        }
        let mut height = info.fast_height;
        if self.has_content
            && (!height.is_finite()
                || keep_boundary_near(height, self.remaining())
                || keep_boundary_near(height, self.content_height()))
        {
            height = self.exact_keep_height(index, range);
        }
        if self.has_content && height > self.remaining() && height <= self.content_height() {
            self.continue_on_new_page(range.section)?;
        }
        Ok(())
    }

    fn exact_keep_height(&self, index: usize, range: &SectionRange) -> f32 {
        let blocks = &self.document.blocks;
        let end = self.keep_chains[index].end;
        let mut height = 0.0_f32;
        let mut next = index;
        while next < end {
            height += self.measured[next].full_height();
            next += 1;
        }
        if end < range.blocks.end {
            height += match &self.measured[end] {
                Measured::Text(metrics) => {
                    metrics.leading_height(if blocks[end].flags.widow_control {
                        MINIMUM_SPLIT_LINES
                    } else {
                        1
                    })
                }
                Measured::Atomic { height } => *height,
            };
        }
        height
    }

    fn place_text(
        &mut self,
        metrics: &TextMetrics,
        keep_lines: bool,
        widow_control: bool,
        id: &ObjectId,
        range: &SectionRange,
    ) -> Result<Vec<Fragment>, DocsightError> {
        let total = metrics.line_count();
        let full_height = metrics.full_height();
        if keep_lines
            && self.has_content
            && full_height > self.remaining()
            && full_height <= self.content_height()
        {
            self.continue_on_new_page(range.section)?;
        }
        let mut fragments = Vec::new();
        let mut start = 0_usize;
        loop {
            let before = if start == 0 {
                metrics.space_before
            } else {
                0.0
            };
            let fitting = lines_that_fit(self.remaining() - before, metrics.line_height);
            let rest = total - start;
            if fitting >= rest {
                fragments.push(Fragment {
                    page: self.current_page()?,
                    top: self.cursor,
                    lines: start..total,
                });
                self.cursor += metrics.fragment_height(&(start..total));
                self.has_content = true;
                return Ok(fragments);
            }
            let mut split = fitting;
            if widow_control {
                split = split.min(rest.saturating_sub(MINIMUM_SPLIT_LINES));
                if split < MINIMUM_SPLIT_LINES {
                    split = 0;
                }
            }
            if split == 0 {
                if self.has_content {
                    self.continue_on_new_page(range.section)?;
                    continue;
                }
                split = fitting.clamp(1, rest);
                let page = self.current_page()?;
                if fitting == 0 {
                    self.warnings.push(object_warning(
                        "DOCX_BLOCK_TALLER_THAN_PAGE",
                        id,
                        page,
                        format!("a line of block {id} is taller than the content area and overflows the page"),
                        "the line geometry extends beyond the page bottom edge",
                    ));
                } else {
                    self.warnings.push(object_warning(
                        "DOCX_WIDOW_CONTROL_RELAXED",
                        id,
                        page,
                        format!(
                            "block {id} cannot keep two lines on each side of the page break on page {page}"
                        ),
                        "the page break leaves fewer lines than widow and orphan control requires",
                    ));
                }
                if split >= rest {
                    fragments.push(Fragment {
                        page,
                        top: self.cursor,
                        lines: start..total,
                    });
                    self.cursor += metrics.fragment_height(&(start..total));
                    self.has_content = true;
                    return Ok(fragments);
                }
            }
            let end = start + split;
            fragments.push(Fragment {
                page: self.current_page()?,
                top: self.cursor,
                lines: start..end,
            });
            start = end;
            self.continue_on_new_page(range.section)?;
        }
    }

    fn place_atomic(
        &mut self,
        height: f32,
        kind: BlockKind,
        id: &ObjectId,
        range: &SectionRange,
    ) -> Result<Vec<Fragment>, DocsightError> {
        if self.has_content && height > self.remaining() {
            self.continue_on_new_page(range.section)?;
            if kind == BlockKind::Table {
                let page = self.current_page()?;
                self.warnings.push(object_warning(
                    "DOCX_PAGINATION_BLOCK_GRANULAR",
                    id,
                    page,
                    format!("table {id} does not fit the remaining space and moved to page {page} as a whole"),
                    "tables are not split between rows, so the previous page ends earlier than in Word",
                ));
            }
        }
        let page = self.current_page()?;
        if height > self.content_height() {
            self.warnings.push(object_warning(
                "DOCX_BLOCK_TALLER_THAN_PAGE",
                id,
                page,
                format!("block {id} is taller than the content area and overflows the page"),
                "the block geometry extends beyond the page bottom edge",
            ));
        }
        let fragment = Fragment {
            page,
            top: self.cursor,
            lines: 0..0,
        };
        self.cursor += height;
        self.has_content = true;
        Ok(vec![fragment])
    }
}

fn lines_that_fit(available: f32, line_height: f32) -> usize {
    if !available.is_finite() || available <= 0.0 || line_height <= 0.0 {
        return 0;
    }
    let count = (available / line_height + LINE_FIT_TOLERANCE).floor();
    if count <= 0.0 {
        0
    } else if count >= usize::MAX as f32 {
        usize::MAX
    } else {
        count as usize
    }
}

fn object_warning(
    code: &str,
    id: &ObjectId,
    page: u32,
    message: String,
    effect: &str,
) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Warning,
        message,
        effect: effect.to_owned(),
        object: Some(id.clone()),
        page: Some(page),
        occurrences: None,
    }
}

fn page_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "layout page count".to_owned(),
        limit: u64::from(MAX_LAYOUT_PAGES),
    }
}
