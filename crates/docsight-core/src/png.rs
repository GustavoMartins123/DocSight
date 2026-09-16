use crate::DocsightError;
use std::io::{Read, Write};

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const CHANNELS: usize = 3;
const DEFLATE_LEVEL: u32 = 6;

pub fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let pixel_width = usize::try_from(width).map_err(|_| png_limit())?;
    let row_bytes = pixel_width.checked_mul(CHANNELS).ok_or_else(png_limit)?;
    let expected = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(png_limit)?;
    if pixels.len() != expected {
        return Err(DocsightError::InvalidArgument {
            message: "PNG pixel buffer does not match the declared dimensions".to_owned(),
        });
    }
    let mut raw = Vec::with_capacity(expected.saturating_add(height as usize));
    let mut previous = vec![0_u8; row_bytes];
    let mut candidates: [Vec<u8>; 5] = Default::default();
    for row in pixels.chunks_exact(row_bytes) {
        let mut best = 0_u8;
        let mut best_index = 0_usize;
        let mut best_score = u64::MAX;
        for (index, candidate) in candidates.iter_mut().enumerate() {
            candidate.clear();
            filter_row(index as u8, row, &previous, candidate);
            let score = filter_score(candidate);
            if score < best_score {
                best_score = score;
                best = index as u8;
                best_index = index;
            }
        }
        raw.push(best);
        raw.extend_from_slice(&candidates[best_index][..]);
        previous.copy_from_slice(row);
    }
    let compressed = deflate(&raw)?;
    let mut png = SIGNATURE.to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header)?;
    append_chunk(&mut png, b"IDAT", &compressed)?;
    append_chunk(&mut png, b"IEND", &[])?;
    Ok(png)
}

fn filter_row(filter: u8, row: &[u8], previous: &[u8], output: &mut Vec<u8>) {
    match filter {
        1 => {
            for (index, byte) in row.iter().enumerate() {
                let left = if index >= CHANNELS {
                    row[index - CHANNELS]
                } else {
                    0
                };
                output.push(byte.wrapping_sub(left));
            }
        }
        2 => {
            for (index, byte) in row.iter().enumerate() {
                output.push(byte.wrapping_sub(previous[index]));
            }
        }
        3 => {
            for (index, byte) in row.iter().enumerate() {
                let left = if index >= CHANNELS {
                    row[index - CHANNELS]
                } else {
                    0
                };
                let average = ((u16::from(left) + u16::from(previous[index])) / 2) as u8;
                output.push(byte.wrapping_sub(average));
            }
        }
        4 => {
            for (index, byte) in row.iter().enumerate() {
                let left = if index >= CHANNELS {
                    row[index - CHANNELS]
                } else {
                    0
                };
                let upper = previous[index];
                let upper_left = if index >= CHANNELS {
                    previous[index - CHANNELS]
                } else {
                    0
                };
                output.push(byte.wrapping_sub(paeth(left, upper, upper_left)));
            }
        }
        _ => output.extend_from_slice(row),
    }
}

fn paeth(left: u8, upper: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let upper = i32::from(upper);
    let upper_left = i32::from(upper_left);
    let estimate = left + upper - upper_left;
    let distance_left = (estimate - left).abs();
    let distance_upper = (estimate - upper).abs();
    let distance_upper_left = (estimate - upper_left).abs();
    if distance_left <= distance_upper && distance_left <= distance_upper_left {
        left as u8
    } else if distance_upper <= distance_upper_left {
        upper as u8
    } else {
        upper_left as u8
    }
}

fn filter_score(filtered: &[u8]) -> u64 {
    filtered
        .iter()
        .map(|byte| u64::from((*byte as i8 as i16).unsigned_abs()))
        .sum()
}

fn deflate(bytes: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(DEFLATE_LEVEL));
    encoder
        .write_all(bytes)
        .and_then(|()| encoder.finish())
        .map_err(|error| DocsightError::BackendFailure {
            backend: "deflate".to_owned(),
            message: error.to_string(),
        })
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), DocsightError> {
    let length = u32::try_from(data.len()).map_err(|_| png_limit())?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut checksum = Vec::with_capacity(kind.len() + data.len());
    checksum.extend_from_slice(kind);
    checksum.extend_from_slice(data);
    output.extend_from_slice(&crc32(&checksum).to_be_bytes());
    Ok(())
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

fn png_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "PNG pixel bytes".to_owned(),
        limit: u64::from(u32::MAX),
    }
}

pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_IMAGE_RAW_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?
            .checked_mul(4)?;
        let slice = self.rgba.get(index..index + 4)?;
        Some([slice[0], slice[1], slice[2], slice[3]])
    }
}

struct ImageHeader {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

pub fn decode_png(bytes: &[u8]) -> Result<DecodedImage, DocsightError> {
    if bytes.len() < 8 || bytes[..8] != SIGNATURE {
        return Err(malformed_image("data does not start with a PNG signature"));
    }
    let mut cursor = 8_usize;
    let mut header: Option<ImageHeader> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut transparency: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut ended = false;

    while cursor < bytes.len() {
        let header_end = cursor
            .checked_add(8)
            .ok_or_else(|| malformed_image("chunk header overflows the image"))?;
        if header_end > bytes.len() {
            return Err(malformed_image("truncated PNG chunk header"));
        }
        let length = u32::from_be_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]) as usize;
        let kind = &bytes[cursor + 4..cursor + 8];
        let data_start = cursor + 8;
        let data_end = data_start
            .checked_add(length)
            .ok_or_else(|| malformed_image("chunk length overflows the image"))?;
        if data_end.checked_add(4).is_none_or(|end| end > bytes.len()) {
            return Err(malformed_image("truncated PNG chunk data"));
        }
        let data = &bytes[data_start..data_end];
        match kind {
            b"IHDR" => {
                if data.len() != 13 {
                    return Err(malformed_image("IHDR chunk has an invalid length"));
                }
                header = Some(ImageHeader {
                    width: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
                    height: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
                    bit_depth: data[8],
                    color_type: data[9],
                    interlace: data[12],
                });
            }
            b"PLTE" => {
                if !data.len().is_multiple_of(3) {
                    return Err(malformed_image("PLTE chunk length is not a multiple of 3"));
                }
                palette = data
                    .chunks_exact(3)
                    .map(|rgb| [rgb[0], rgb[1], rgb[2]])
                    .collect();
            }
            b"tRNS" => transparency = data.to_vec(),
            b"IDAT" => {
                if idat.len().saturating_add(data.len()) as u64 > MAX_IMAGE_RAW_BYTES {
                    return Err(image_limit("PNG compressed bytes", MAX_IMAGE_RAW_BYTES));
                }
                idat.extend_from_slice(data);
            }
            b"IEND" => {
                ended = true;
                break;
            }
            _ => {}
        }
        cursor = data_end + 4;
    }

    if !ended {
        return Err(malformed_image("PNG has no IEND chunk"));
    }
    let header = header.ok_or_else(|| malformed_image("PNG has no IHDR chunk"))?;
    if header.width == 0 || header.height == 0 {
        return Err(malformed_image("PNG declares a zero dimension"));
    }
    if u64::from(header.width) * u64::from(header.height) > MAX_IMAGE_PIXELS {
        return Err(image_limit("PNG pixels", MAX_IMAGE_PIXELS));
    }
    if header.interlace != 0 {
        return Err(DocsightError::UnsupportedFeature {
            feature: "interlaced PNG image".to_owned(),
        });
    }
    if header.bit_depth != 8 {
        return Err(DocsightError::UnsupportedFeature {
            feature: format!("PNG bit depth {}", header.bit_depth),
        });
    }
    let channels = match header.color_type {
        0 => 1_usize,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => {
            return Err(DocsightError::UnsupportedFeature {
                feature: format!("PNG color type {other}"),
            });
        }
    };
    if header.color_type == 3 && palette.is_empty() {
        return Err(malformed_image("indexed PNG has no palette"));
    }

    let width = usize::try_from(header.width).map_err(|_| png_limit())?;
    let height = usize::try_from(header.height).map_err(|_| png_limit())?;
    let row_bytes = width.checked_mul(channels).ok_or_else(png_limit)?;
    let expected = row_bytes
        .checked_add(1)
        .and_then(|stride| stride.checked_mul(height))
        .ok_or_else(png_limit)?;

    let mut raw = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(idat.as_slice()).take(expected as u64 + 1);
    decoder
        .read_to_end(&mut raw)
        .map_err(|error| malformed_image(format!("PNG image data is not valid zlib: {error}")))?;
    if raw.len() != expected {
        return Err(malformed_image(
            "PNG image data does not match the declared dimensions",
        ));
    }

    let mut previous = vec![0_u8; row_bytes];
    let mut current = vec![0_u8; row_bytes];
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let start = row * (row_bytes + 1);
        let filter = raw[start];
        current.copy_from_slice(&raw[start + 1..start + 1 + row_bytes]);
        unfilter_row(filter, &mut current, &previous, channels)?;
        expand_row(
            &current,
            header.color_type,
            channels,
            &palette,
            &transparency,
            &mut rgba,
        )?;
        previous.copy_from_slice(&current);
    }

    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        rgba,
    })
}

fn unfilter_row(
    filter: u8,
    row: &mut [u8],
    previous: &[u8],
    channels: usize,
) -> Result<(), DocsightError> {
    match filter {
        0 => {}
        1 => {
            for index in channels..row.len() {
                row[index] = row[index].wrapping_add(row[index - channels]);
            }
        }
        2 => {
            for index in 0..row.len() {
                row[index] = row[index].wrapping_add(previous[index]);
            }
        }
        3 => {
            for index in 0..row.len() {
                let left = if index >= channels {
                    u16::from(row[index - channels])
                } else {
                    0
                };
                let above = u16::from(previous[index]);
                row[index] = row[index].wrapping_add(((left + above) / 2) as u8);
            }
        }
        4 => {
            for index in 0..row.len() {
                let left = if index >= channels {
                    row[index - channels]
                } else {
                    0
                };
                let above = previous[index];
                let upper_left = if index >= channels {
                    previous[index - channels]
                } else {
                    0
                };
                row[index] = row[index].wrapping_add(paeth(left, above, upper_left));
            }
        }
        other => {
            return Err(malformed_image(format!("unknown PNG row filter {other}")));
        }
    }
    Ok(())
}

fn expand_row(
    row: &[u8],
    color_type: u8,
    channels: usize,
    palette: &[[u8; 3]],
    transparency: &[u8],
    rgba: &mut Vec<u8>,
) -> Result<(), DocsightError> {
    for pixel in row.chunks_exact(channels) {
        match color_type {
            0 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            2 => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            3 => {
                let index = usize::from(pixel[0]);
                let color = palette.get(index).ok_or_else(|| {
                    malformed_image("indexed PNG references a missing palette entry")
                })?;
                let alpha = transparency.get(index).copied().unwrap_or(255);
                rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
            }
            4 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            6 => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], pixel[3]]),
            other => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PNG color type {other}"),
                });
            }
        }
    }
    Ok(())
}

fn malformed_image(message: impl Into<String>) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("embedded PNG image is malformed: {}", message.into()),
    }
}

fn image_limit(resource: &str, limit: u64) -> DocsightError {
    DocsightError::ResourceLimit {
        resource: resource.to_owned(),
        limit,
    }
}

#[cfg(test)]
mod tests {
    use super::encode_png;

    fn decode_idat(png: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
            return Err("missing PNG signature".into());
        }
        let mut cursor = 8_usize;
        let mut idat = Vec::new();
        loop {
            let end = cursor.checked_add(8).ok_or("truncated PNG chunk header")?;
            if end > png.len() {
                return Err("truncated PNG chunk header".into());
            }
            let length = u32::from_be_bytes([
                png[cursor],
                png[cursor + 1],
                png[cursor + 2],
                png[cursor + 3],
            ]) as usize;
            let kind = &png[cursor + 4..cursor + 8];
            let data_end = cursor
                .checked_add(8 + length)
                .ok_or("truncated PNG chunk data")?;
            if data_end > png.len() {
                return Err("truncated PNG chunk data".into());
            }
            if kind == b"IDAT" {
                idat.extend_from_slice(&png[cursor + 8..data_end]);
            }
            if kind == b"IEND" {
                break;
            }
            cursor = data_end.checked_add(4).ok_or("truncated PNG chunk")?;
        }
        let mut decoder = flate2::read::ZlibDecoder::new(idat.as_slice());
        let mut raw = Vec::new();
        use std::io::Read as _;
        decoder.read_to_end(&mut raw)?;
        Ok(raw)
    }

    #[test]
    fn encodes_deterministic_lossless_rgb() -> Result<(), Box<dyn std::error::Error>> {
        let pixels: Vec<u8> = (0..48).map(|value| (value * 7) as u8).collect();
        let first = encode_png(4, 4, &pixels)?;
        let second = encode_png(4, 4, &pixels)?;
        assert_eq!(first, second);
        let raw = decode_idat(&first)?;
        assert_eq!(raw.len(), 4 * (1 + 4 * 3));
        let solid = vec![200_u8; 300];
        let encoded = encode_png(10, 10, &solid)?;
        assert!(encoded.len() < solid.len());
        Ok(())
    }

    #[test]
    fn decodes_its_own_encoded_output() -> Result<(), Box<dyn std::error::Error>> {
        let pixels: Vec<u8> = (0..48).map(|value| (value * 7) as u8).collect();
        let encoded = super::encode_png(4, 4, &pixels)?;
        let decoded = super::decode_png(&encoded)?;
        assert_eq!(decoded.width, 4);
        assert_eq!(decoded.height, 4);
        assert_eq!(decoded.rgba.len(), 4 * 4 * 4);
        for index in 0..16 {
            assert_eq!(decoded.rgba[index * 4], pixels[index * 3]);
            assert_eq!(decoded.rgba[index * 4 + 1], pixels[index * 3 + 1]);
            assert_eq!(decoded.rgba[index * 4 + 2], pixels[index * 3 + 2]);
            assert_eq!(decoded.rgba[index * 4 + 3], 255);
        }
        assert_eq!(
            decoded.pixel(0, 0),
            Some([pixels[0], pixels[1], pixels[2], 255])
        );
        assert_eq!(decoded.pixel(4, 0), None);
        Ok(())
    }

    #[test]
    fn rejects_malformed_and_unsupported_images() -> Result<(), Box<dyn std::error::Error>> {
        assert!(super::decode_png(b"not a png").is_err());
        let mut truncated = super::encode_png(2, 2, &[0_u8; 12])?;
        truncated.truncate(20);
        assert!(super::decode_png(&truncated).is_err());
        let mut interlaced = super::encode_png(2, 2, &[0_u8; 12])?;
        interlaced[8 + 8 + 12] = 1;
        assert!(matches!(
            super::decode_png(&interlaced),
            Err(crate::DocsightError::UnsupportedFeature { .. })
                | Err(crate::DocsightError::MalformedDocument { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_mismatched_pixel_buffers() -> Result<(), Box<dyn std::error::Error>> {
        assert!(encode_png(2, 2, &[0_u8; 11]).is_err());
        assert!(encode_png(2, 2, &[0_u8; 13]).is_err());
        assert!(encode_png(2, 2, &[0_u8; 12]).is_ok());
        Ok(())
    }
}
