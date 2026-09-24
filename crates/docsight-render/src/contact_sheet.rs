use docsight_core::{Diagnostic, DocsightError, write_all};
use docsight_ingest::ingest_docx;
use docsight_pdf::PdfDocument;
use serde::Serialize;
use std::path::Path;

pub const CONTACT_SHEET_MAX_PAGES: usize = 64;
pub const CONTACT_SHEET_MAX_OUTPUT_PIXELS: u64 = 16_000_000;
pub const CONTACT_SHEET_MAX_SOURCE_PIXELS: u64 = 100_000_000;
pub const CONTACT_SHEET_COLUMNS: usize = 4;
pub const CONTACT_SHEET_DEFAULT_DPI: u16 = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ContactSheetLimits {
    pub max_pages: usize,
    pub max_output_pixels: u64,
    pub max_source_pixels: u64,
}

pub const CONTACT_SHEET_LIMITS: ContactSheetLimits = ContactSheetLimits {
    max_pages: CONTACT_SHEET_MAX_PAGES,
    max_output_pixels: CONTACT_SHEET_MAX_OUTPUT_PIXELS,
    max_source_pixels: CONTACT_SHEET_MAX_SOURCE_PIXELS,
};

const OUTPUT_MARGIN: u32 = 12;
const CELL_GAP: u32 = 8;
const CELL_WIDTH: u32 = 256;
const CELL_HEIGHT: u32 = 336;
const LABEL_HEIGHT: u32 = 24;
const CONTENT_HEIGHT: u32 = CELL_HEIGHT - LABEL_HEIGHT;
const THUMBNAIL_MAX_WIDTH: u32 = 240;
const THUMBNAIL_MAX_HEIGHT: u32 = 300;
const CHANNELS: usize = 3;

const BACKGROUND: Color = Color {
    red: 224,
    green: 224,
    blue: 224,
};
const CELL_BACKGROUND: Color = Color {
    red: 255,
    green: 255,
    blue: 255,
};
const CELL_BORDER: Color = Color {
    red: 96,
    green: 96,
    blue: 96,
};
const LABEL_BACKGROUND: Color = Color {
    red: 48,
    green: 48,
    blue: 48,
};
const LABEL_TEXT: Color = Color {
    red: 255,
    green: 255,
    blue: 255,
};

#[derive(Clone, Copy)]
struct Color {
    red: u8,
    green: u8,
    blue: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactSheetRequest {
    pub pages: Vec<u32>,
    pub dpi: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContactSheetMetadata {
    pub pages: Vec<u32>,
    pub labels: Vec<String>,
    pub limits: ContactSheetLimits,
    pub dpi: u16,
    pub columns: usize,
    pub rows: usize,
    pub width_px: u32,
    pub height_px: u32,
    pub thumbnail_max_width_px: u32,
    pub thumbnail_max_height_px: u32,
    pub media_type: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContactSheet {
    pub metadata: ContactSheetMetadata,
    pub warnings: Vec<Diagnostic>,
    png: Vec<u8>,
    pixels: Vec<u8>,
}

impl ContactSheet {
    pub fn png(&self) -> &[u8] {
        &self.png
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn write(&self, path: &Path) -> Result<(), DocsightError> {
        write_all(path, &self.png)
    }
}

struct Thumbnail {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub fn render_contact_sheet(
    source: &docsight_core::DocumentSource,
    request: &ContactSheetRequest,
) -> Result<ContactSheet, DocsightError> {
    render_contact_sheet_with_password(source, request, b"")
}

pub fn render_contact_sheet_with_password(
    source: &docsight_core::DocumentSource,
    request: &ContactSheetRequest,
    password: &[u8],
) -> Result<ContactSheet, DocsightError> {
    let pages = normalize_pages(&request.pages)?;
    match source.format() {
        docsight_core::DocumentFormat::Pdf => {
            let document = PdfDocument::open_with_password(source, password)?;
            build_contact_sheet(pages, request.dpi, |page| {
                let raster = document.rasterize(page, request.dpi, None)?;
                Ok(crate::from_raster(raster))
            })
        }
        docsight_core::DocumentFormat::Docx if !password.is_empty() => {
            Err(DocsightError::InvalidArgument {
                message: "a PDF password cannot be applied to a DOCX document".to_owned(),
            })
        }
        docsight_core::DocumentFormat::Docx => {
            let laid_out = ingest_docx(source)?;
            build_contact_sheet(pages, request.dpi, |page| {
                let page_record = laid_out
                    .pages
                    .iter()
                    .find(|candidate| candidate.number == page)
                    .ok_or_else(|| DocsightError::ObjectNotFound {
                        object: format!("page {page}"),
                    })?;
                let mut warnings = laid_out.document.warnings.clone();
                let images = crate::decode_page_images(source, page_record, &mut warnings);
                crate::docx_raster::rasterize_docx_page(
                    page_record,
                    request.dpi,
                    None,
                    warnings,
                    &images,
                )
            })
        }
    }
}

fn build_contact_sheet<F>(
    pages: Vec<u32>,
    dpi: u16,
    mut render_page: F,
) -> Result<ContactSheet, DocsightError>
where
    F: FnMut(u32) -> Result<crate::RenderedImage, DocsightError>,
{
    let mut warnings = Vec::new();
    let mut thumbnails = Vec::with_capacity(pages.len());
    let mut source_pixels = 0_u64;
    for page in pages.iter().copied() {
        let rendered = render_page(page)?;
        let page_pixels = u64::from(rendered.metadata.width_px)
            .checked_mul(u64::from(rendered.metadata.height_px))
            .ok_or_else(source_pixel_limit)?;
        source_pixels = source_pixels
            .checked_add(page_pixels)
            .ok_or_else(source_pixel_limit)?;
        if source_pixels > CONTACT_SHEET_MAX_SOURCE_PIXELS {
            return Err(source_pixel_limit());
        }
        warnings.extend(rendered.warnings.iter().cloned());
        thumbnails.push(thumbnail(&rendered)?);
    }
    let (width, height, columns, rows) = output_dimensions(pages.len())?;
    let labels = pages.iter().map(|page| format!("p. {page}")).collect();
    let pixels = compose(&pages, &thumbnails, width, height, columns)?;
    let png = docsight_core::encode_png(width, height, &pixels)?;
    Ok(ContactSheet {
        metadata: ContactSheetMetadata {
            pages,
            labels,
            limits: CONTACT_SHEET_LIMITS,
            dpi,
            columns,
            rows,
            width_px: width,
            height_px: height,
            thumbnail_max_width_px: THUMBNAIL_MAX_WIDTH,
            thumbnail_max_height_px: THUMBNAIL_MAX_HEIGHT,
            media_type: "image/png",
        },
        warnings,
        png,
        pixels,
    })
}

fn normalize_pages(pages: &[u32]) -> Result<Vec<u32>, DocsightError> {
    if pages.is_empty() {
        return Err(DocsightError::InvalidArgument {
            message: "contact sheet requires at least one page".to_owned(),
        });
    }
    if pages.len() > CONTACT_SHEET_MAX_PAGES {
        return Err(DocsightError::ResourceLimit {
            resource: "contact sheet pages".to_owned(),
            limit: CONTACT_SHEET_MAX_PAGES as u64,
        });
    }
    if pages.contains(&0) {
        return Err(DocsightError::InvalidArgument {
            message: "contact sheet pages must be 1-based".to_owned(),
        });
    }
    let mut normalized = pages.to_vec();
    normalized.sort_unstable();
    if normalized.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(DocsightError::InvalidArgument {
            message: "contact sheet pages must be unique".to_owned(),
        });
    }
    Ok(normalized)
}

fn output_dimensions(page_count: usize) -> Result<(u32, u32, usize, usize), DocsightError> {
    if page_count == 0 {
        return Err(DocsightError::InvalidArgument {
            message: "contact sheet requires at least one page".to_owned(),
        });
    }
    let columns = page_count.min(CONTACT_SHEET_COLUMNS);
    let rows = page_count
        .checked_add(columns - 1)
        .ok_or_else(output_pixel_limit)?
        / columns;
    let columns_u32 = u32::try_from(columns).map_err(|_| output_pixel_limit())?;
    let rows_u32 = u32::try_from(rows).map_err(|_| output_pixel_limit())?;
    let columns_extent = columns_u32
        .checked_mul(CELL_WIDTH)
        .and_then(|value| value.checked_add(columns_u32.saturating_sub(1).checked_mul(CELL_GAP)?))
        .ok_or_else(output_pixel_limit)?;
    let rows_extent = rows_u32
        .checked_mul(CELL_HEIGHT)
        .and_then(|value| value.checked_add(rows_u32.saturating_sub(1).checked_mul(CELL_GAP)?))
        .ok_or_else(output_pixel_limit)?;
    let width = OUTPUT_MARGIN
        .checked_mul(2)
        .and_then(|value| value.checked_add(columns_extent))
        .ok_or_else(output_pixel_limit)?;
    let height = OUTPUT_MARGIN
        .checked_mul(2)
        .and_then(|value| value.checked_add(rows_extent))
        .ok_or_else(output_pixel_limit)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(output_pixel_limit)?;
    if pixels > CONTACT_SHEET_MAX_OUTPUT_PIXELS {
        return Err(output_pixel_limit());
    }
    Ok((width, height, columns, rows))
}

fn thumbnail(rendered: &crate::RenderedImage) -> Result<Thumbnail, DocsightError> {
    let source_width = rendered.metadata.width_px;
    let source_height = rendered.metadata.height_px;
    if source_width == 0 || source_height == 0 {
        return Err(DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: "page raster has zero dimensions".to_owned(),
        });
    }
    let (width, height) = thumbnail_dimensions(source_width, source_height);
    let mut pixels = vec![0_u8; checked_pixel_bytes(width, height)?];
    copy_resized(
        rendered.pixels(),
        source_width,
        source_height,
        &mut pixels,
        width,
        height,
    )?;
    Ok(Thumbnail {
        width,
        height,
        pixels,
    })
}

fn thumbnail_dimensions(source_width: u32, source_height: u32) -> (u32, u32) {
    if source_width <= THUMBNAIL_MAX_WIDTH && source_height <= THUMBNAIL_MAX_HEIGHT {
        return (source_width, source_height);
    }
    let width_limited = u64::from(THUMBNAIL_MAX_WIDTH) * u64::from(source_height)
        <= u64::from(THUMBNAIL_MAX_HEIGHT) * u64::from(source_width);
    if width_limited {
        let width = THUMBNAIL_MAX_WIDTH;
        let height = scaled_dimension(source_height, source_width, width);
        (width, height)
    } else {
        let height = THUMBNAIL_MAX_HEIGHT;
        let width = scaled_dimension(source_width, source_height, height);
        (width, height)
    }
}

fn scaled_dimension(source: u32, other: u32, target: u32) -> u32 {
    let numerator = u64::from(source) * u64::from(target);
    let denominator = u64::from(other);
    let scaled = ((numerator + denominator / 2) / denominator).max(1);
    if scaled > u64::from(u32::MAX) {
        u32::MAX
    } else {
        scaled as u32
    }
}

fn checked_pixel_bytes(width: u32, height: u32) -> Result<usize, DocsightError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(output_pixel_limit)?;
    let bytes = pixels
        .checked_mul(CHANNELS as u64)
        .ok_or_else(output_pixel_limit)?;
    usize::try_from(bytes).map_err(|_| output_pixel_limit())
}

fn copy_resized(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target: &mut [u8],
    target_width: u32,
    target_height: u32,
) -> Result<(), DocsightError> {
    let expected_source = checked_pixel_bytes(source_width, source_height)?;
    if source.len() != expected_source {
        return Err(DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: "page raster buffer does not match its dimensions".to_owned(),
        });
    }
    for y in 0..target_height {
        let source_y = u64::from(y) * u64::from(source_height) / u64::from(target_height);
        for x in 0..target_width {
            let source_x = u64::from(x) * u64::from(source_width) / u64::from(target_width);
            let source_offset =
                usize::try_from((source_y * u64::from(source_width) + source_x) * CHANNELS as u64)
                    .map_err(|_| output_pixel_limit())?;
            let target_offset = usize::try_from(
                (u64::from(y) * u64::from(target_width) + u64::from(x)) * CHANNELS as u64,
            )
            .map_err(|_| output_pixel_limit())?;
            let source_end = source_offset
                .checked_add(CHANNELS)
                .ok_or_else(output_pixel_limit)?;
            let target_end = target_offset
                .checked_add(CHANNELS)
                .ok_or_else(output_pixel_limit)?;
            let source_slice = source
                .get(source_offset..source_end)
                .ok_or_else(output_pixel_limit)?;
            let target_slice = target
                .get_mut(target_offset..target_end)
                .ok_or_else(output_pixel_limit)?;
            target_slice.copy_from_slice(source_slice);
        }
    }
    Ok(())
}

fn compose(
    pages: &[u32],
    thumbnails: &[Thumbnail],
    width: u32,
    height: u32,
    columns: usize,
) -> Result<Vec<u8>, DocsightError> {
    let mut pixels = vec![0_u8; checked_pixel_bytes(width, height)?];
    for value in pixels.chunks_exact_mut(CHANNELS) {
        value[0] = BACKGROUND.red;
        value[1] = BACKGROUND.green;
        value[2] = BACKGROUND.blue;
    }
    for (index, thumbnail) in thumbnails.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let column = u32::try_from(column).map_err(|_| output_pixel_limit())?;
        let row = u32::try_from(row).map_err(|_| output_pixel_limit())?;
        let x = OUTPUT_MARGIN
            .checked_add(
                column
                    .checked_mul(CELL_WIDTH + CELL_GAP)
                    .ok_or_else(output_pixel_limit)?,
            )
            .ok_or_else(output_pixel_limit)?;
        let y = OUTPUT_MARGIN
            .checked_add(
                row.checked_mul(CELL_HEIGHT + CELL_GAP)
                    .ok_or_else(output_pixel_limit)?,
            )
            .ok_or_else(output_pixel_limit)?;
        let page = pages.get(index).copied().ok_or_else(output_pixel_limit)?;
        draw_cell(&mut pixels, width, height, x, y, page)?;
        let image_x = x
            .checked_add((CELL_WIDTH - thumbnail.width) / 2)
            .ok_or_else(output_pixel_limit)?;
        let image_y = y
            .checked_add(LABEL_HEIGHT)
            .and_then(|value| value.checked_add((CONTENT_HEIGHT - thumbnail.height) / 2))
            .ok_or_else(output_pixel_limit)?;
        copy_at(&mut pixels, width, height, image_x, image_y, thumbnail)?;
    }
    Ok(pixels)
}

fn draw_cell(
    pixels: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    page: u32,
) -> Result<(), DocsightError> {
    fill_rect(
        pixels,
        canvas_width,
        canvas_height,
        x,
        y,
        CELL_WIDTH,
        CELL_HEIGHT,
        CELL_BACKGROUND,
    )?;
    fill_rect(
        pixels,
        canvas_width,
        canvas_height,
        x,
        y,
        CELL_WIDTH,
        LABEL_HEIGHT,
        LABEL_BACKGROUND,
    )?;
    let mut label = String::from("p. ");
    label.push_str(&page.to_string());
    draw_label(pixels, canvas_width, canvas_height, x + 6, y + 5, &label)?;
    for offset in 0..CELL_WIDTH {
        set_pixel(
            pixels,
            canvas_width,
            canvas_height,
            x + offset,
            y,
            CELL_BORDER,
        )?;
        set_pixel(
            pixels,
            canvas_width,
            canvas_height,
            x + offset,
            y + CELL_HEIGHT - 1,
            CELL_BORDER,
        )?;
    }
    for offset in 0..CELL_HEIGHT {
        set_pixel(
            pixels,
            canvas_width,
            canvas_height,
            x,
            y + offset,
            CELL_BORDER,
        )?;
        set_pixel(
            pixels,
            canvas_width,
            canvas_height,
            x + CELL_WIDTH - 1,
            y + offset,
            CELL_BORDER,
        )?;
    }
    Ok(())
}

fn draw_label(
    pixels: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    label: &str,
) -> Result<(), DocsightError> {
    let glyph_width = 5_u32;
    let mut cursor = x;
    for character in label.chars() {
        let pattern = label_glyph(character).ok_or_else(|| DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: format!("unsupported contact sheet label character {character:?}"),
        })?;
        for (row, bits) in pattern.iter().enumerate() {
            for column in 0..glyph_width {
                if bits & (1 << (glyph_width - column - 1)) == 0 {
                    continue;
                }
                let row = u32::try_from(row).map_err(|_| output_pixel_limit())?;
                fill_rect(
                    pixels,
                    canvas_width,
                    canvas_height,
                    cursor + column,
                    y + row,
                    1,
                    1,
                    LABEL_TEXT,
                )?;
            }
        }
        cursor = cursor
            .checked_add(glyph_width + 1)
            .ok_or_else(output_pixel_limit)?;
    }
    Ok(())
}

fn label_glyph(character: char) -> Option<&'static [u8; 7]> {
    match character {
        '0' => Some(&[0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e]),
        '1' => Some(&[0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e]),
        '2' => Some(&[0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f]),
        '3' => Some(&[0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e]),
        '4' => Some(&[0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02]),
        '5' => Some(&[0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e]),
        '6' => Some(&[0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e]),
        '7' => Some(&[0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
        '8' => Some(&[0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e]),
        '9' => Some(&[0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c]),
        'p' => Some(&[0x00, 0x00, 0x16, 0x19, 0x19, 0x1a, 0x1c]),
        '.' => Some(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c]),
        ' ' => Some(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
    pixels: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Color,
) -> Result<(), DocsightError> {
    let right = x.checked_add(width).ok_or_else(output_pixel_limit)?;
    let bottom = y.checked_add(height).ok_or_else(output_pixel_limit)?;
    if right > canvas_width || bottom > canvas_height {
        return Err(DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: "contact sheet cell exceeds output bounds".to_owned(),
        });
    }
    for row in y..bottom {
        for column in x..right {
            set_pixel(pixels, canvas_width, canvas_height, column, row, color)?;
        }
    }
    Ok(())
}

fn set_pixel(
    pixels: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    color: Color,
) -> Result<(), DocsightError> {
    if x >= canvas_width || y >= canvas_height {
        return Err(DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: "contact sheet pixel exceeds output bounds".to_owned(),
        });
    }
    let offset =
        usize::try_from((u64::from(y) * u64::from(canvas_width) + u64::from(x)) * CHANNELS as u64)
            .map_err(|_| output_pixel_limit())?;
    let end = offset
        .checked_add(CHANNELS)
        .ok_or_else(output_pixel_limit)?;
    let slot = pixels.get_mut(offset..end).ok_or_else(output_pixel_limit)?;
    slot[0] = color.red;
    slot[1] = color.green;
    slot[2] = color.blue;
    Ok(())
}

fn copy_at(
    pixels: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    thumbnail: &Thumbnail,
) -> Result<(), DocsightError> {
    let right = x
        .checked_add(thumbnail.width)
        .ok_or_else(output_pixel_limit)?;
    let bottom = y
        .checked_add(thumbnail.height)
        .ok_or_else(output_pixel_limit)?;
    if right > canvas_width || bottom > canvas_height {
        return Err(DocsightError::BackendFailure {
            backend: "contact-sheet".to_owned(),
            message: "contact sheet thumbnail exceeds cell bounds".to_owned(),
        });
    }
    for row in 0..thumbnail.height {
        let source_offset =
            usize::try_from((u64::from(row) * u64::from(thumbnail.width)) * CHANNELS as u64)
                .map_err(|_| output_pixel_limit())?;
        let target_y = y.checked_add(row).ok_or_else(output_pixel_limit)?;
        let target_offset = usize::try_from(
            (u64::from(target_y) * u64::from(canvas_width) + u64::from(x)) * CHANNELS as u64,
        )
        .map_err(|_| output_pixel_limit())?;
        let length = usize::try_from(u64::from(thumbnail.width) * CHANNELS as u64)
            .map_err(|_| output_pixel_limit())?;
        let source_end = source_offset
            .checked_add(length)
            .ok_or_else(output_pixel_limit)?;
        let target_end = target_offset
            .checked_add(length)
            .ok_or_else(output_pixel_limit)?;
        let source = thumbnail
            .pixels
            .get(source_offset..source_end)
            .ok_or_else(output_pixel_limit)?;
        let target = pixels
            .get_mut(target_offset..target_end)
            .ok_or_else(output_pixel_limit)?;
        target.copy_from_slice(source);
    }
    Ok(())
}

fn source_pixel_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "contact sheet source pixels".to_owned(),
        limit: CONTACT_SHEET_MAX_SOURCE_PIXELS,
    }
}

fn output_pixel_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "contact sheet output pixels".to_owned(),
        limit: CONTACT_SHEET_MAX_OUTPUT_PIXELS,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONTACT_SHEET_MAX_PAGES, ContactSheetRequest, normalize_pages, output_dimensions,
        thumbnail_dimensions,
    };
    use docsight_core::DocsightError;

    #[test]
    fn normalizes_pages_into_canonical_order() -> Result<(), DocsightError> {
        assert_eq!(normalize_pages(&[3, 1, 2])?, vec![1, 2, 3]);
        assert!(matches!(
            normalize_pages(&[1, 1]),
            Err(DocsightError::InvalidArgument { .. })
        ));
        assert!(matches!(
            normalize_pages(&[0]),
            Err(DocsightError::InvalidArgument { .. })
        ));
        assert!(matches!(
            normalize_pages(&vec![1; CONTACT_SHEET_MAX_PAGES + 1]),
            Err(DocsightError::ResourceLimit { .. })
        ));
        Ok(())
    }

    #[test]
    fn derives_bounded_output_dimensions() -> Result<(), DocsightError> {
        let (width, height, columns, rows) = output_dimensions(5)?;
        assert_eq!((columns, rows), (4, 2));
        assert!(width > 0 && height > 0);
        let (width, height, columns, rows) = output_dimensions(1)?;
        assert_eq!((width, height, columns, rows), (280, 360, 1, 1));
        assert_eq!(thumbnail_dimensions(612, 792), (232, 300));
        assert_eq!(thumbnail_dimensions(792, 612), (240, 185));
        Ok(())
    }

    #[test]
    fn request_keeps_explicit_page_selection() {
        let request = ContactSheetRequest {
            pages: vec![2, 1],
            dpi: 72,
        };
        assert_eq!(request.pages, vec![2, 1]);
    }
}
