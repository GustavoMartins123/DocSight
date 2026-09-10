use crate::DocsightError;
use std::io::Write;

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
    fn rejects_mismatched_pixel_buffers() -> Result<(), Box<dyn std::error::Error>> {
        assert!(encode_png(2, 2, &[0_u8; 11]).is_err());
        assert!(encode_png(2, 2, &[0_u8; 13]).is_err());
        assert!(encode_png(2, 2, &[0_u8; 12]).is_ok());
        Ok(())
    }
}
