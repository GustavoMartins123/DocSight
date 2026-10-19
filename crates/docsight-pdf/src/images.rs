use crate::raster::MAX_RASTER_PIXELS;
use crate::syntax::{Value, malformed};
use docsight_core::{DecodedImage, DocsightError};

pub(crate) const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_CHANNELS: usize = 4;

#[derive(Clone, Debug)]
pub(crate) enum ImageColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Indexed {
        base: Box<ImageColorSpace>,
        high: u16,
        table: Vec<u8>,
    },
    CalGray {
        white: [f32; 3],
        gamma: f32,
    },
    CalRgb {
        white: [f32; 3],
        gamma: [f32; 3],
        matrix: [f32; 9],
    },
    Lab {
        white: [f32; 3],
        range: [f32; 4],
    },
}

impl ImageColorSpace {
    pub(crate) fn components(&self) -> usize {
        match self {
            Self::Gray | Self::CalGray { .. } | Self::Indexed { .. } => 1,
            Self::Rgb | Self::CalRgb { .. } | Self::Lab { .. } => 3,
            Self::Cmyk => 4,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CodecImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) components: u8,
    pub(crate) data: Vec<u8>,
    pub(crate) alpha: Option<Vec<u8>>,
    pub(crate) color: CodecColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodecColor {
    Gray,
    Rgb,
    Cmyk,
}

pub(crate) fn image_error(message: impl Into<String>) -> DocsightError {
    malformed(format!("PDF image {}", message.into()))
}

pub(crate) fn unsupported_image(feature: impl Into<String>) -> DocsightError {
    DocsightError::UnsupportedFeature {
        feature: format!("PDF image {}", feature.into()),
    }
}

pub(crate) fn image_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "PDF image pixels".to_owned(),
        limit: MAX_RASTER_PIXELS,
    }
}

pub(crate) fn check_dimensions(width: i64, height: i64) -> Result<(u32, u32), DocsightError> {
    if width <= 0 || height <= 0 {
        return Err(image_error("dimensions must be positive"));
    }
    let width = u32::try_from(width).map_err(|_| image_error("width is out of range"))?;
    let height = u32::try_from(height).map_err(|_| image_error("height is out of range"))?;
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(image_limit());
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(image_limit)?;
    if pixels > MAX_RASTER_PIXELS {
        return Err(image_limit());
    }
    Ok((width, height))
}

pub(crate) fn unpack_samples(
    data: &[u8],
    width: u32,
    height: u32,
    bits: u8,
    components: usize,
) -> Result<Vec<u16>, DocsightError> {
    if components == 0 || components > MAX_CHANNELS {
        return Err(image_error("component count is unsupported"));
    }
    let max_sample = sample_max(bits)?;
    let row_bits = (width as usize)
        .checked_mul(components)
        .and_then(|value| value.checked_mul(usize::from(bits)))
        .ok_or_else(image_limit)?;
    let row_bytes = row_bits.div_ceil(8);
    let expected = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(image_limit)?;
    if data.len() != expected {
        return Err(image_error(format!(
            "sample data has {} bytes, expected {expected}",
            data.len()
        )));
    }
    let sample_count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|value| value.checked_mul(components))
        .ok_or_else(image_limit)?;
    let mut samples = Vec::with_capacity(sample_count);
    match bits {
        8 => samples.extend(data.iter().copied().map(u16::from)),
        16 => {
            for pair in data.chunks_exact(2) {
                samples.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
        }
        _ => {
            for row in data.chunks_exact(row_bytes) {
                for index in 0..width as usize * components {
                    let bit_offset = index * usize::from(bits);
                    let byte = row[bit_offset / 8];
                    let shift = 8 - usize::from(bits) - (bit_offset % 8);
                    let value = u16::from(byte >> shift) & max_sample;
                    samples.push(value);
                }
            }
        }
    }
    Ok(samples)
}

pub(crate) fn sample_max(bits: u8) -> Result<u16, DocsightError> {
    match bits {
        1 => Ok(1),
        2 => Ok(3),
        4 => Ok(15),
        8 => Ok(255),
        16 => Ok(u16::MAX),
        _ => Err(image_error(format!(
            "BitsPerComponent {bits} is unsupported"
        ))),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodeRange {
    pub(crate) min: f32,
    pub(crate) max: f32,
}

impl DecodeRange {
    pub(crate) fn apply(self, sample: u16, max_sample: u16) -> f32 {
        let normalized = f32::from(sample) / f32::from(max_sample);
        self.min + normalized * (self.max - self.min)
    }
}

pub(crate) fn decode_ranges(
    values: &[f32],
    components: usize,
) -> Result<Vec<DecodeRange>, DocsightError> {
    let expected = components * 2;
    if values.len() != expected {
        return Err(image_error(format!(
            "Decode has {} entries, expected {expected}",
            values.len()
        )));
    }
    Ok(values
        .chunks_exact(2)
        .map(|pair| DecodeRange {
            min: pair[0],
            max: pair[1],
        })
        .collect())
}

pub(crate) fn default_decode_ranges(components: usize) -> Vec<DecodeRange> {
    vec![DecodeRange { min: 0.0, max: 1.0 }; components]
}

pub(crate) fn parse_decode_values(values: &[Value]) -> Result<Vec<f32>, DocsightError> {
    let mut decoded = Vec::with_capacity(values.len());
    for value in values {
        let number = match value {
            Value::Int(value) => *value as f32,
            Value::Real(value) => *value,
            _ => return Err(image_error("Decode must contain numbers")),
        };
        if !number.is_finite() {
            return Err(image_error("Decode contains a non-finite value"));
        }
        decoded.push(number);
    }
    Ok(decoded)
}

pub(crate) struct ColorKeyRange {
    pub min: u16,
    pub max: u16,
}

pub(crate) struct SampleRender<'a> {
    pub width: u32,
    pub height: u32,
    pub samples: &'a [u16],
    pub bits: u8,
    pub space: &'a ImageColorSpace,
    pub ranges: &'a [DecodeRange],
    pub alpha: Option<&'a [u8]>,
    pub color_key: Option<&'a [ColorKeyRange]>,
}

pub(crate) fn rgba_from_samples(request: SampleRender<'_>) -> Result<Vec<u8>, DocsightError> {
    let SampleRender {
        width,
        height,
        samples,
        bits,
        space,
        ranges,
        alpha,
        color_key,
    } = request;
    let max_sample = sample_max(bits)?;
    let components = space.components();
    if samples.len() != width as usize * height as usize * components {
        return Err(image_error(
            "decoded sample buffer does not match the image",
        ));
    }
    if ranges.len() != components {
        return Err(image_error("Decode ranges do not match the color space"));
    }
    if let Some(values) = alpha
        && values.len() != width as usize * height as usize
    {
        return Err(image_error(
            "alpha buffer does not match the image dimensions",
        ));
    }
    if let Some(keys) = color_key
        && keys.len() != components
    {
        return Err(image_error(
            "Mask color-key ranges do not match the color space",
        ));
    }
    let quad = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(height as usize))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(image_limit)?;
    let mut rgba = Vec::with_capacity(quad);
    for (pixel, chunk) in samples.chunks_exact(components).enumerate() {
        let mut mapped = [0.0; MAX_CHANNELS];
        for ((value, sample), range) in mapped.iter_mut().zip(chunk).zip(ranges) {
            *value = range.apply(*sample, max_sample);
        }
        let rgb = color_to_rgb(space, &mapped)?;
        let mut alpha_value = match alpha {
            Some(values) => values
                .get(pixel)
                .copied()
                .ok_or_else(|| image_error("alpha buffer does not match the image dimensions"))?,
            None => 255,
        };
        if let Some(keys) = color_key {
            let keyed = chunk
                .iter()
                .zip(keys)
                .all(|(sample, range)| *sample >= range.min && *sample <= range.max);
            if keyed {
                alpha_value = 0;
            }
        }
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha_value]);
    }
    Ok(rgba)
}

fn color_to_rgb(
    space: &ImageColorSpace,
    values: &[f32; MAX_CHANNELS],
) -> Result<[u8; 3], DocsightError> {
    Ok(match space {
        ImageColorSpace::Gray => {
            let value = to_byte(values[0]);
            [value, value, value]
        }
        ImageColorSpace::Rgb => [to_byte(values[0]), to_byte(values[1]), to_byte(values[2])],
        ImageColorSpace::Cmyk => cmyk_to_rgb(values[0], values[1], values[2], values[3]),
        ImageColorSpace::Indexed { base, high, table } => {
            let index = values[0].round().clamp(0.0, f32::from(*high)) as usize;
            let base_components = base.components();
            let offset = index.checked_mul(base_components).ok_or_else(image_limit)?;
            let end = offset
                .checked_add(base_components)
                .ok_or_else(image_limit)?;
            if end > table.len() {
                return Err(image_error(
                    "Indexed palette is shorter than its high index",
                ));
            }
            let mut base_values = [0.0; MAX_CHANNELS];
            for (value, sample) in base_values.iter_mut().zip(&table[offset..end]) {
                *value = f32::from(*sample) / 255.0;
            }
            color_to_rgb(base, &base_values)?
        }
        ImageColorSpace::CalGray { white, gamma } => {
            let linear = values[0].clamp(0.0, 1.0).powf(gamma.clamp(0.01, 100.0));
            xyz_to_rgb(
                white[0] * linear,
                white[1] * linear,
                white[2] * linear,
                *white,
            )
        }
        ImageColorSpace::CalRgb {
            white,
            gamma,
            matrix,
        } => {
            let linear = [
                values[0].clamp(0.0, 1.0).powf(gamma[0].clamp(0.01, 100.0)),
                values[1].clamp(0.0, 1.0).powf(gamma[1].clamp(0.01, 100.0)),
                values[2].clamp(0.0, 1.0).powf(gamma[2].clamp(0.01, 100.0)),
            ];
            let x = matrix[0] * linear[0] + matrix[3] * linear[1] + matrix[6] * linear[2];
            let y = matrix[1] * linear[0] + matrix[4] * linear[1] + matrix[7] * linear[2];
            let z = matrix[2] * linear[0] + matrix[5] * linear[1] + matrix[8] * linear[2];
            xyz_to_rgb(x, y, z, *white)
        }
        ImageColorSpace::Lab { white, range } => {
            let l = values[0].clamp(0.0, 1.0) * 100.0;
            let a = range[0] + values[1].clamp(0.0, 1.0) * (range[1] - range[0]);
            let b = range[2] + values[2].clamp(0.0, 1.0) * (range[3] - range[2]);
            let fy = (l + 16.0) / 116.0;
            let fx = fy + a / 500.0;
            let fz = fy - b / 200.0;
            let x = white[0] * lab_curve(fx);
            let y = white[1] * lab_curve(fy);
            let z = white[2] * lab_curve(fz);
            xyz_to_rgb(x, y, z, *white)
        }
    })
}

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [u8; 3] {
    [
        to_byte((1.0 - c) * (1.0 - k)),
        to_byte((1.0 - m) * (1.0 - k)),
        to_byte((1.0 - y) * (1.0 - k)),
    ]
}

fn lab_curve(value: f32) -> f32 {
    let delta = 6.0 / 29.0;
    if value > delta {
        value * value * value
    } else {
        3.0 * delta * delta * (value - 4.0 / 29.0)
    }
}

fn xyz_to_rgb(x: f32, y: f32, z: f32, white: [f32; 3]) -> [u8; 3] {
    let adapted_x = if white[0] > 0.0 {
        x * 1.047_811_5 / white[0]
    } else {
        x
    };
    let adapted_y = if white[1] > 0.0 { y / white[1] } else { y };
    let adapted_z = if white[2] > 0.0 {
        z * 0.918_415_5 / white[2]
    } else {
        z
    };
    let r = 3.240_454_2 * adapted_x - 1.537_138_5 * adapted_y - 0.498_531_4 * adapted_z;
    let g = -0.969_266 * adapted_x + 1.876_010_8 * adapted_y + 0.041_556 * adapted_z;
    let b = 0.055_643_4 * adapted_x - 0.204_025_9 * adapted_y + 1.057_225_2 * adapted_z;
    [
        to_byte(gamma_encode(r)),
        to_byte(gamma_encode(g)),
        to_byte(gamma_encode(b)),
    ]
}

fn gamma_encode(value: f32) -> f32 {
    let value = value.max(0.0);
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(crate) fn rgba_to_decoded(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<DecodedImage, DocsightError> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(image_error(
            "RGBA buffer does not match the image dimensions",
        ));
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

pub(crate) fn codec_to_rgba(
    image: &CodecImage,
    alpha: Option<&[u8]>,
) -> Result<Vec<u8>, DocsightError> {
    let width = image.width;
    let height = image.height;
    let components = usize::from(image.components);
    if components == 0 || components > MAX_CHANNELS {
        return Err(image_error("codec produced an unsupported component count"));
    }
    let expected = width as usize * height as usize * components;
    if image.data.len() != expected {
        return Err(image_error(
            "codec sample count does not match the image dimensions",
        ));
    }
    let alpha = alpha.or(image.alpha.as_deref());
    if let Some(values) = alpha
        && values.len() != width as usize * height as usize
    {
        return Err(image_error(
            "alpha buffer does not match the image dimensions",
        ));
    }
    let mut rgba = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(image_limit)?,
    );
    for (pixel, chunk) in image.data.chunks_exact(components).enumerate() {
        let rgb = match image.color {
            CodecColor::Gray => [chunk[0], chunk[0], chunk[0]],
            CodecColor::Rgb => [chunk[0], chunk[1], chunk[2]],
            CodecColor::Cmyk => {
                let black = u16::from(255 - chunk[3]);
                let channel = |value: u8| {
                    u8::try_from(u16::from(255 - value) * black / 255)
                        .map_err(|_| image_error("CMYK channel is out of range"))
                };
                [channel(chunk[0])?, channel(chunk[1])?, channel(chunk[2])?]
            }
        };
        let alpha_value = match alpha {
            Some(values) => values
                .get(pixel)
                .copied()
                .ok_or_else(|| image_error("alpha buffer does not match the image dimensions"))?,
            None => 255,
        };
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha_value]);
    }
    Ok(rgba)
}

pub(crate) fn decode_jpx(data: &[u8]) -> Result<CodecImage, DocsightError> {
    let image = hayro_jpeg2000::Image::new(data, &hayro_jpeg2000::DecodeSettings::default())
        .map_err(|error| image_error(format!("JPXDecode failed: {error}")))?;
    let width = image.width();
    let height = image.height();
    check_dimensions(i64::from(width), i64::from(height))?;
    let color = match image.color_space() {
        hayro_jpeg2000::ColorSpace::Gray => CodecColor::Gray,
        hayro_jpeg2000::ColorSpace::RGB => CodecColor::Rgb,
        hayro_jpeg2000::ColorSpace::CMYK => CodecColor::Cmyk,
        hayro_jpeg2000::ColorSpace::Unknown { num_channels } => {
            return Err(unsupported_image(format!(
                "JPXDecode color space with {num_channels} channels"
            )));
        }
        hayro_jpeg2000::ColorSpace::Icc { num_channels, .. } => {
            return Err(unsupported_image(format!(
                "JPXDecode ICC-based color space with {num_channels} channels"
            )));
        }
    };
    let channels = usize::from(image.color_space().num_channels());
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(image_error(
            "JPXDecode produced an unsupported component count",
        ));
    }
    let has_alpha = image.has_alpha();
    let mut context = hayro_jpeg2000::DecoderContext::default();
    let decoded = image
        .decode(&mut context)
        .map_err(|error| image_error(format!("JPXDecode failed: {error}")))?;
    let decoded_data = decoded.data_u8();
    let stride = channels + usize::from(has_alpha);
    let color_samples = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(channels))
        .ok_or_else(image_limit)?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(stride))
        .ok_or_else(image_limit)?;
    if decoded_data.len() != expected {
        return Err(image_error(
            "JPXDecode sample count does not match the image dimensions",
        ));
    }
    let (data, alpha) = if has_alpha {
        let mut data = Vec::with_capacity(color_samples);
        let mut values = Vec::with_capacity(width as usize * height as usize);
        for pixel in decoded_data.chunks_exact(stride) {
            data.extend_from_slice(&pixel[..channels]);
            values.push(pixel[channels]);
        }
        (data, Some(values))
    } else {
        (decoded_data, None)
    };
    let components =
        u8::try_from(channels).map_err(|_| image_error("JPXDecode produced too many channels"))?;
    Ok(CodecImage {
        width,
        height,
        components,
        data,
        alpha,
        color,
    })
}

struct GrayCollector {
    pixels: Vec<u8>,
    limit: u64,
}

impl GrayCollector {
    fn new(limit: u64) -> Self {
        Self {
            pixels: Vec::new(),
            limit,
        }
    }

    fn push(&mut self, black: bool, count: u64) -> Result<(), DocsightError> {
        let additional = usize::try_from(count).map_err(|_| image_limit())?;
        if self.pixels.len() as u64 + count > self.limit {
            return Err(image_limit());
        }
        let value = if black { 0 } else { 255 };
        self.pixels.extend(std::iter::repeat_n(value, additional));
        Ok(())
    }
}

impl hayro_jbig2::Decoder for GrayCollector {
    fn push_pixel(&mut self, black: bool) {
        let value = if black { 0 } else { 255 };
        self.pixels.push(value);
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let value = if black { 0 } else { 255 };
        self.pixels
            .extend(std::iter::repeat_n(value, chunk_count as usize * 8));
    }

    fn next_line(&mut self) {}
}

pub(crate) fn decode_jbig2(
    data: &[u8],
    globals: Option<&[u8]>,
    declared_width: u32,
    declared_height: u32,
) -> Result<CodecImage, DocsightError> {
    let image = hayro_jbig2::Image::new_embedded(data, globals)
        .map_err(|error| image_error(format!("JBIG2Decode failed: {error}")))?;
    let width = image.width();
    let height = image.height();
    check_dimensions(i64::from(width), i64::from(height))?;
    if width != declared_width || height != declared_height {
        return Err(image_error(format!(
            "JBIG2Decode decodes {width}x{height}, image declares {declared_width}x{declared_height}"
        )));
    }
    let limit = u64::from(width) * u64::from(height);
    let mut collector = GrayCollector::new(limit);
    image
        .decode(&mut collector)
        .map_err(|error| image_error(format!("JBIG2Decode failed: {error}")))?;
    let expected = usize::try_from(limit).map_err(|_| image_limit())?;
    if collector.pixels.len() < expected {
        return Err(image_error(
            "JBIG2Decode produced fewer samples than the image declares",
        ));
    }
    collector.pixels.truncate(expected);
    Ok(CodecImage {
        width,
        height,
        components: 1,
        data: collector.pixels,
        alpha: None,
        color: CodecColor::Gray,
    })
}

impl hayro_ccitt::Decoder for GrayCollector {
    fn push_pixels(&mut self, white: bool, count: u32) {
        let _ = self.push(!white, u64::from(count));
    }

    fn next_line(&mut self) {}
}

pub(crate) fn decode_ccitt(
    data: &[u8],
    settings: hayro_ccitt::DecodeSettings,
    declared_width: u32,
    declared_height: u32,
) -> Result<CodecImage, DocsightError> {
    let width = settings.columns;
    let height = settings.rows;
    check_dimensions(i64::from(width), i64::from(height))?;
    if width != declared_width || height != declared_height {
        return Err(image_error(format!(
            "CCITTFaxDecode decodes {width}x{height}, image declares {declared_width}x{declared_height}"
        )));
    }
    let limit = u64::from(width) * u64::from(height);
    let mut collector = GrayCollector::new(limit);
    let mut context = hayro_ccitt::DecoderContext::new(settings);
    hayro_ccitt::decode(data, &mut collector, &mut context)
        .map_err(|error| image_error(format!("CCITTFaxDecode failed: {error}")))?;
    let expected = usize::try_from(limit).map_err(|_| image_limit())?;
    if collector.pixels.len() < expected {
        return Err(image_error(
            "CCITTFaxDecode produced fewer samples than the image declares",
        ));
    }
    collector.pixels.truncate(expected);
    Ok(CodecImage {
        width,
        height,
        components: 1,
        data: collector.pixels,
        alpha: None,
        color: CodecColor::Gray,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CodecColor, CodecImage, DecodeRange, ImageColorSpace, SampleRender, codec_to_rgba,
        default_decode_ranges, rgba_from_samples,
    };
    use docsight_core::DocsightError;

    #[test]
    fn codec_images_reject_short_and_long_alpha_buffers() {
        let mut image = CodecImage {
            width: 1,
            height: 1,
            components: 3,
            data: vec![255, 0, 0],
            alpha: None,
            color: CodecColor::Rgb,
        };
        for alpha in [vec![], vec![0, 255]] {
            assert!(matches!(
                codec_to_rgba(&image, Some(&alpha)),
                Err(DocsightError::MalformedDocument { .. })
            ));
            image.alpha = Some(alpha);
            assert!(matches!(
                codec_to_rgba(&image, None),
                Err(DocsightError::MalformedDocument { .. })
            ));
            image.alpha = None;
        }
    }

    #[test]
    fn indexed_palettes_match_direct_conversion_for_every_base_space() -> Result<(), DocsightError>
    {
        let white = [0.950_47, 1.0, 1.088_83];
        for base in [
            ImageColorSpace::Gray,
            ImageColorSpace::Rgb,
            ImageColorSpace::Cmyk,
            ImageColorSpace::CalGray { white, gamma: 2.2 },
            ImageColorSpace::CalRgb {
                white,
                gamma: [2.2, 1.8, 2.0],
                matrix: [
                    0.412_456_4,
                    0.212_672_9,
                    0.019_333_9,
                    0.357_576_1,
                    0.715_152_2,
                    0.119_192,
                    0.180_437_5,
                    0.072_175,
                    0.950_304_1,
                ],
            },
            ImageColorSpace::Lab {
                white,
                range: [-128.0, 127.0, -128.0, 127.0],
            },
        ] {
            let table: Vec<u8> = [0, 64, 128, 255]
                .into_iter()
                .flat_map(|value| std::iter::repeat_n(value, base.components()))
                .collect();
            let samples: Vec<u16> = table.iter().copied().map(u16::from).collect();
            let direct = rgba_from_samples(SampleRender {
                width: 4,
                height: 1,
                samples: &samples,
                bits: 8,
                space: &base,
                ranges: &default_decode_ranges(base.components()),
                alpha: None,
                color_key: None,
            })?;
            let indexed = ImageColorSpace::Indexed {
                base: Box::new(base),
                high: 3,
                table,
            };
            let actual = rgba_from_samples(SampleRender {
                width: 4,
                height: 1,
                samples: &[0, 1, 2, 3],
                bits: 8,
                space: &indexed,
                ranges: &[DecodeRange {
                    min: 0.0,
                    max: 255.0,
                }],
                alpha: None,
                color_key: None,
            })?;
            assert_eq!(actual, direct, "{indexed:?}");
        }
        Ok(())
    }

    #[test]
    fn repeated_rgb_pixels_keep_every_channel_and_alpha() -> Result<(), DocsightError> {
        let samples = [0, 64, 255, 255, 128, 0].repeat(4096);
        let alpha = [255, 128].repeat(4096);
        let actual = rgba_from_samples(SampleRender {
            width: 128,
            height: 64,
            samples: &samples,
            bits: 8,
            space: &ImageColorSpace::Rgb,
            ranges: &default_decode_ranges(3),
            alpha: Some(&alpha),
            color_key: None,
        })?;
        assert_eq!(actual, [0, 64, 255, 255, 255, 128, 0, 128].repeat(4096));
        Ok(())
    }
}
