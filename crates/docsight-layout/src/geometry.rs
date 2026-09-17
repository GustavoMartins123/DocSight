use docsight_core::{DocsightError, Document, Section};
use std::ops::Range;

const DEFAULT_PAGE_WIDTH_PT: f32 = 612.0;
const DEFAULT_PAGE_HEIGHT_PT: f32 = 792.0;
const DEFAULT_MARGIN_PT: f32 = 72.0;
const DEFAULT_HEADER_FOOTER_DISTANCE_PT: f32 = 36.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SectionGeometry {
    pub page_width: f32,
    pub page_height: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub header_distance: f32,
    pub footer_distance: f32,
}

impl SectionGeometry {
    pub fn from_section(section: &Section) -> Result<Self, DocsightError> {
        let geometry = Self {
            page_width: section.page_width_pt.unwrap_or(DEFAULT_PAGE_WIDTH_PT),
            page_height: section.page_height_pt.unwrap_or(DEFAULT_PAGE_HEIGHT_PT),
            margin_top: section.margin_top_pt.unwrap_or(DEFAULT_MARGIN_PT).abs(),
            margin_bottom: section.margin_bottom_pt.unwrap_or(DEFAULT_MARGIN_PT).abs(),
            margin_left: section.margin_left_pt.unwrap_or(DEFAULT_MARGIN_PT),
            margin_right: section.margin_right_pt.unwrap_or(DEFAULT_MARGIN_PT),
            header_distance: section
                .header_distance_pt
                .unwrap_or(DEFAULT_HEADER_FOOTER_DISTANCE_PT)
                .abs(),
            footer_distance: section
                .footer_distance_pt
                .unwrap_or(DEFAULT_HEADER_FOOTER_DISTANCE_PT)
                .abs(),
        };
        let values = [
            geometry.page_width,
            geometry.page_height,
            geometry.margin_top,
            geometry.margin_bottom,
            geometry.margin_left,
            geometry.margin_right,
            geometry.header_distance,
            geometry.footer_distance,
        ];
        if values.iter().any(|value| !value.is_finite())
            || geometry.content_width() <= 0.0
            || geometry.content_height() <= 0.0
        {
            return Err(DocsightError::MalformedDocument {
                message: format!(
                    "DOCX section {} margins leave no positive page content area",
                    section.section_index
                ),
            });
        }
        Ok(geometry)
    }

    pub fn content_width(&self) -> f32 {
        self.page_width - self.margin_left - self.margin_right
    }

    pub fn content_bottom(&self) -> f32 {
        self.page_height - self.margin_bottom
    }

    pub fn content_height(&self) -> f32 {
        self.content_bottom() - self.margin_top
    }

    pub fn same_page_size(&self, other: &Self) -> bool {
        (self.page_width - other.page_width).abs() < 0.01
            && (self.page_height - other.page_height).abs() < 0.01
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SectionRange {
    pub section: usize,
    pub blocks: Range<usize>,
}

pub(crate) fn section_ranges(document: &Document) -> Result<Vec<SectionRange>, DocsightError> {
    Ok(document
        .section_block_ranges()?
        .into_iter()
        .enumerate()
        .map(|(section, blocks)| SectionRange { section, blocks })
        .collect())
}
