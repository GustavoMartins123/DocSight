use crate::syntax::{StreamValue, Value, malformed};
use docsight_core::DocsightError;
use std::collections::BTreeMap;
use std::io::{Read, Take};

const MAX_FLATE_OUTPUT_BYTES: u64 = docsight_core::MAX_INSPECT_BYTES;
const MAX_PREDICTOR_COLUMNS: usize = 1 << 20;

pub(crate) fn decode_stream(stream: &StreamValue) -> Result<Vec<u8>, DocsightError> {
    decode_filtered(&stream.dict, &stream.data, &indirect_unsupported)
}

pub(crate) fn decode_stream_without_final_filter(
    stream: &StreamValue,
) -> Result<Vec<u8>, DocsightError> {
    let mut dict = stream.dict.clone();
    let filter_value = dict.get("Filter").cloned();
    match filter_value {
        None | Some(Value::Null) => {}
        Some(Value::Name(_)) => {
            dict.insert("Filter".to_owned(), Value::Null);
            dict.insert("DecodeParms".to_owned(), Value::Null);
        }
        Some(Value::Array(values)) if !values.is_empty() => {
            let filter_count = values.len();
            let parms_value = dict.get("DecodeParms").cloned();
            let mut kept = values;
            kept.pop();
            if kept.is_empty() {
                dict.insert("Filter".to_owned(), Value::Null);
            } else {
                dict.insert("Filter".to_owned(), Value::Array(kept));
            }
            match parms_value {
                Some(Value::Array(mut parms)) if parms.len() == filter_count => {
                    parms.pop();
                    dict.insert("DecodeParms".to_owned(), Value::Array(parms));
                }
                _ => {
                    dict.insert("DecodeParms".to_owned(), Value::Null);
                }
            }
        }
        Some(Value::Ref(_)) => {
            return Err(DocsightError::UnsupportedFeature {
                feature: "PDF stream Filter given indirectly".to_owned(),
            });
        }
        _ => {
            return Err(crate::syntax::malformed(
                "stream Filter must be a name or an array",
            ));
        }
    }
    decode_filtered(&dict, &stream.data, &indirect_unsupported)
}

fn indirect_unsupported(_: &Value) -> Result<Value, DocsightError> {
    Err(DocsightError::UnsupportedFeature {
        feature: "PDF stream Filter or DecodeParms given indirectly".to_owned(),
    })
}

fn decode_filtered(
    dict: &BTreeMap<String, Value>,
    data: &[u8],
    resolve: &impl Fn(&Value) -> Result<Value, DocsightError>,
) -> Result<Vec<u8>, DocsightError> {
    let filter = match dict.get("Filter") {
        None | Some(Value::Null) => return Ok(data.to_vec()),
        Some(Value::Ref(_)) => {
            let resolved = resolve(dict.get("Filter").unwrap_or(&Value::Null))?;
            return decode_filtered(&with_key(dict, "Filter", resolved), data, resolve);
        }
        Some(value) => value.clone(),
    };
    let parms = match dict.get("DecodeParms") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::Ref(_)) => resolve(dict.get("DecodeParms").unwrap_or(&Value::Null))?,
        Some(value) => value.clone(),
    };

    let names = match &filter {
        Value::Name(name) => vec![name.clone()],
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Name(name) => Ok(name.clone()),
                _ => Err(malformed("stream Filter array must contain names")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(malformed("stream Filter must be a name or an array")),
    };
    let parms = match parms {
        Value::Null => vec![Value::Null; names.len()],
        Value::Dict(dict) => {
            let mut list = vec![Value::Null; names.len()];
            if let Some(first) = list.first_mut() {
                *first = Value::Dict(dict);
            }
            list
        }
        Value::Array(values) if values.len() == names.len() => values,
        Value::Array(_) => return Err(malformed("DecodeParms length must match Filter length")),
        _ => return Err(malformed("DecodeParms must be a dictionary or an array")),
    };

    let mut output = data.to_vec();
    for (name, parm) in names.iter().zip(parms) {
        let parm = match parm {
            Value::Ref(_) => resolve(&parm)?,
            other => other,
        };
        output = match name.as_str() {
            "FlateDecode" | "Fl" => inflate_zlib(&output)?,
            "ASCII85Decode" | "A85" => ascii85_decode(&output)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&output)?,
            "LZWDecode" | "LZW" => {
                let early_change = match &parm {
                    Value::Dict(dict) => match dict.get("EarlyChange") {
                        Some(Value::Ref(_)) => {
                            match resolve(dict.get("EarlyChange").unwrap_or(&Value::Null))? {
                                Value::Int(value) => value != 0,
                                Value::Null => true,
                                _ => return Err(malformed("EarlyChange must be an integer")),
                            }
                        }
                        Some(Value::Int(value)) => *value != 0,
                        Some(Value::Null) | None => true,
                        _ => return Err(malformed("EarlyChange must be an integer")),
                    },
                    Value::Null => true,
                    _ => return Err(malformed("DecodeParms must be a dictionary")),
                };
                lzw_decode(&output, early_change)?
            }
            "RunLengthDecode" | "RL" => run_length_decode(&output)?,
            other => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF stream filter {other}"),
                });
            }
        };
        output = apply_predictor(&parm, output, resolve)?;
    }
    Ok(output)
}

fn with_key(dict: &BTreeMap<String, Value>, key: &str, value: Value) -> BTreeMap<String, Value> {
    let mut updated = dict.clone();
    updated.insert(key.to_owned(), value);
    updated
}

fn integer(
    parms: &BTreeMap<String, Value>,
    key: &str,
    default: i64,
    resolve: &impl Fn(&Value) -> Result<Value, DocsightError>,
) -> Result<i64, DocsightError> {
    let Some(value) = parms.get(key) else {
        return Ok(default);
    };
    let value = match value {
        Value::Ref(_) => resolve(value)?,
        other => other.clone(),
    };
    match value {
        Value::Int(number) => Ok(number),
        Value::Null => Ok(default),
        _ => Err(malformed(format!("DecodeParms {key} must be an integer"))),
    }
}

fn apply_predictor(
    parms: &Value,
    data: Vec<u8>,
    resolve: &impl Fn(&Value) -> Result<Value, DocsightError>,
) -> Result<Vec<u8>, DocsightError> {
    let Value::Dict(parms) = parms else {
        return Ok(data);
    };
    let predictor = integer(parms, "Predictor", 1, resolve)?;
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = integer(parms, "Colors", 1, resolve)?;
    let bits = integer(parms, "BitsPerComponent", 8, resolve)?;
    let columns = integer(parms, "Columns", 1, resolve)?;
    if !(1..=32).contains(&colors)
        || !matches!(bits, 1 | 2 | 4 | 8 | 16)
        || !(1..=MAX_PREDICTOR_COLUMNS as i64).contains(&columns)
    {
        return Err(malformed("DecodeParms predictor geometry is out of range"));
    }
    let colors = colors as usize;
    let bits = bits as usize;
    let columns = columns as usize;
    let bytes_per_pixel = (colors * bits).div_ceil(8).max(1);
    let row_length = (colors * bits * columns).div_ceil(8);

    match predictor {
        2 => apply_tiff_predictor(data, colors, bits, row_length),
        10..=15 => apply_png_predictor(data, bytes_per_pixel, row_length),
        other => Err(DocsightError::UnsupportedFeature {
            feature: format!("PDF stream predictor {other}"),
        }),
    }
}

fn apply_tiff_predictor(
    mut data: Vec<u8>,
    colors: usize,
    bits: usize,
    row_length: usize,
) -> Result<Vec<u8>, DocsightError> {
    if bits != 8 {
        return Err(DocsightError::UnsupportedFeature {
            feature: format!("PDF TIFF predictor with {bits} bits per component"),
        });
    }
    if row_length == 0 || !data.len().is_multiple_of(row_length) {
        return Err(malformed(
            "TIFF predictor data is not a whole number of rows",
        ));
    }
    for row in data.chunks_mut(row_length) {
        for index in colors..row.len() {
            row[index] = row[index].wrapping_add(row[index - colors]);
        }
    }
    Ok(data)
}

fn apply_png_predictor(
    data: Vec<u8>,
    bytes_per_pixel: usize,
    row_length: usize,
) -> Result<Vec<u8>, DocsightError> {
    if row_length == 0 {
        return Err(malformed("PNG predictor row length must be positive"));
    }
    let stride = row_length + 1;
    if !data.len().is_multiple_of(stride) {
        return Err(malformed(
            "PNG predictor data is not a whole number of rows",
        ));
    }
    let rows = data.len() / stride;
    let mut output = vec![0_u8; rows * row_length];
    let mut previous = vec![0_u8; row_length];
    for row in 0..rows {
        let tag = data[row * stride];
        let source = &data[row * stride + 1..row * stride + stride];
        let start = row * row_length;
        let (done, current) = output.split_at_mut(start);
        let _ = done;
        let current = &mut current[..row_length];
        current.copy_from_slice(source);
        match tag {
            0 => {}
            1 => {
                for index in bytes_per_pixel..row_length {
                    current[index] = current[index].wrapping_add(current[index - bytes_per_pixel]);
                }
            }
            2 => {
                for index in 0..row_length {
                    current[index] = current[index].wrapping_add(previous[index]);
                }
            }
            3 => {
                for index in 0..row_length {
                    let left = if index >= bytes_per_pixel {
                        u16::from(current[index - bytes_per_pixel])
                    } else {
                        0
                    };
                    let up = u16::from(previous[index]);
                    current[index] = current[index].wrapping_add(((left + up) / 2) as u8);
                }
            }
            4 => {
                for index in 0..row_length {
                    let left = if index >= bytes_per_pixel {
                        current[index - bytes_per_pixel]
                    } else {
                        0
                    };
                    let up = previous[index];
                    let up_left = if index >= bytes_per_pixel {
                        previous[index - bytes_per_pixel]
                    } else {
                        0
                    };
                    current[index] = current[index].wrapping_add(paeth(left, up, up_left));
                }
            }
            other => {
                return Err(malformed(format!("invalid PNG predictor tag {other}")));
            }
        }
        previous.copy_from_slice(current);
    }
    Ok(output)
}

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let predictor = i16::from(left) + i16::from(up) - i16::from(up_left);
    let distance_left = (predictor - i16::from(left)).abs();
    let distance_up = (predictor - i16::from(up)).abs();
    let distance_up_left = (predictor - i16::from(up_left)).abs();
    if distance_left <= distance_up && distance_left <= distance_up_left {
        left
    } else if distance_up <= distance_up_left {
        up
    } else {
        up_left
    }
}

pub(crate) fn inflate_zlib(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let bounded: Take<flate2::read::ZlibDecoder<&[u8]>> =
        flate2::read::ZlibDecoder::new(data).take(MAX_FLATE_OUTPUT_BYTES + 1);
    let mut decoder = bounded;
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("FlateDecode stream is invalid: {error}"),
        })?;
    if output.len() as u64 > MAX_FLATE_OUTPUT_BYTES {
        return Err(DocsightError::ResourceLimit {
            resource: "decoded PDF content bytes".to_owned(),
            limit: MAX_FLATE_OUTPUT_BYTES,
        });
    }
    Ok(output)
}

fn lzw_decode(data: &[u8], early_change: bool) -> Result<Vec<u8>, DocsightError> {
    let mut reader = LzwCodeReader::new(data);
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = (0..256).map(|value| vec![value as u8]).collect();
    table.push(Vec::new());
    table.push(Vec::new());
    let mut previous: Option<Vec<u8>> = None;
    while let Some(code) = reader.next_code()? {
        match code {
            256 => {
                table.truncate(258);
                previous = None;
                reader.width = 9;
            }
            257 => break,
            _ => {
                let entry = if (code as usize) < table.len() {
                    table[code as usize].clone()
                } else if code as usize == table.len() {
                    match &previous {
                        Some(previous) if !previous.is_empty() => {
                            let mut entry = previous.clone();
                            entry.push(previous[0]);
                            entry
                        }
                        _ => {
                            return Err(malformed("LZWDecode stream references an undefined code"));
                        }
                    }
                } else {
                    return Err(malformed("LZWDecode stream references an undefined code"));
                };
                out.extend_from_slice(&entry);
                if out.len() as u64 > MAX_FLATE_OUTPUT_BYTES {
                    return Err(DocsightError::ResourceLimit {
                        resource: "decoded PDF content bytes".to_owned(),
                        limit: MAX_FLATE_OUTPUT_BYTES,
                    });
                }
                if let Some(previous) = &previous
                    && table.len() < 4096
                {
                    let mut next = previous.clone();
                    next.push(entry[0]);
                    table.push(next);
                }
                previous = Some(entry);
            }
        }
        reader.observe_table(table.len(), early_change)?;
    }
    Ok(out)
}

struct LzwCodeReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
    width: u8,
}

impl<'a> LzwCodeReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
            width: 9,
        }
    }

    fn next_code(&mut self) -> Result<Option<u16>, DocsightError> {
        let total_bits = self
            .data
            .len()
            .checked_mul(8)
            .ok_or_else(|| malformed("LZWDecode stream is too large"))?;
        if self.bit_offset + usize::from(self.width) > total_bits {
            if total_bits - self.bit_offset < 8 {
                return Ok(None);
            }
            return Err(malformed("LZWDecode stream ends inside a code"));
        }
        let mut code = 0_u16;
        for _ in 0..self.width {
            let byte = self.data[self.bit_offset / 8];
            let bit = (byte >> (7 - (self.bit_offset % 8))) & 1;
            code = (code << 1) | u16::from(bit);
            self.bit_offset += 1;
        }
        Ok(Some(code))
    }

    fn observe_table(&mut self, table_len: usize, early_change: bool) -> Result<(), DocsightError> {
        let offset = usize::from(early_change);
        while self.width < 12 && table_len >= (1_usize << self.width).saturating_sub(offset) {
            self.width += 1;
        }
        Ok(())
    }
}

fn run_length_decode(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < data.len() {
        let length = data[index];
        index += 1;
        match length {
            128 => return Ok(out),
            0..=127 => {
                let count = usize::from(length) + 1;
                let end = index
                    .checked_add(count)
                    .ok_or_else(|| malformed("RunLengthDecode stream overflows"))?;
                if end > data.len() {
                    return Err(malformed("RunLengthDecode stream is truncated"));
                }
                out.extend_from_slice(&data[index..end]);
                index = end;
            }
            _ => {
                let Some(byte) = data.get(index) else {
                    return Err(malformed("RunLengthDecode stream is truncated"));
                };
                index += 1;
                let count = 257 - usize::from(length);
                out.extend(std::iter::repeat_n(*byte, count));
            }
        }
        if out.len() as u64 > MAX_FLATE_OUTPUT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "decoded PDF content bytes".to_owned(),
                limit: MAX_FLATE_OUTPUT_BYTES,
            });
        }
    }
    Ok(out)
}

fn ascii_hex_decode(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for byte in data {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if *byte == b'>' {
            break;
        }
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return Err(malformed("ASCIIHexDecode stream has a non-hex byte")),
        };
        match high.take() {
            None => high = Some(nibble),
            Some(first) => out.push((first << 4) | nibble),
        }
    }
    if let Some(first) = high {
        out.push(first << 4);
    }
    Ok(out)
}

fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut body = data;
    if body.starts_with(b"<~") {
        body = &body[2..];
    }
    let mut out = Vec::new();
    let mut group = [0_u8; 5];
    let mut filled = 0_usize;
    let mut finished = false;
    let mut index = 0_usize;
    while index < body.len() {
        let byte = body[index];
        index += 1;
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'~' {
            finished = true;
            break;
        }
        if byte == b'z' && filled == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&byte) {
            return Err(malformed("ASCII85Decode stream has an invalid byte"));
        }
        group[filled] = byte - b'!';
        filled += 1;
        if filled == 5 {
            out.extend_from_slice(&ascii85_group(&group, 5)?);
            filled = 0;
        }
        if out.len() as u64 > MAX_FLATE_OUTPUT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "decoded PDF content bytes".to_owned(),
                limit: MAX_FLATE_OUTPUT_BYTES,
            });
        }
    }
    let _ = finished;
    if filled == 1 {
        return Err(malformed("ASCII85Decode stream ends with a single symbol"));
    }
    if filled > 1 {
        let mut padded = group;
        for slot in padded.iter_mut().skip(filled) {
            *slot = 84;
        }
        let decoded = ascii85_group(&padded, 5)?;
        out.extend_from_slice(&decoded[..filled - 1]);
    }
    Ok(out)
}

fn ascii85_group(group: &[u8; 5], len: usize) -> Result<[u8; 4], DocsightError> {
    let mut value = 0_u32;
    for symbol in group.iter().take(len) {
        value = value
            .checked_mul(85)
            .and_then(|value| value.checked_add(u32::from(*symbol)))
            .ok_or_else(|| malformed("ASCII85Decode group overflows 32 bits"))?;
    }
    Ok(value.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::lzw_decode;
    use docsight_core::DocsightError;

    fn pack_lzw_codes(codes: &[(u16, u8)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut current = 0_u8;
        let mut filled = 0_u8;
        for &(code, width) in codes {
            for bit in (0..width).rev() {
                current = (current << 1) | u8::from((code >> bit) & 1 != 0);
                filled += 1;
                if filled == 8 {
                    bytes.push(current);
                    current = 0;
                    filled = 0;
                }
            }
        }
        if filled > 0 {
            bytes.push(current << (8 - filled));
        }
        bytes
    }

    #[test]
    fn lzw_clear_restores_nine_bit_codes_after_every_width_transition() -> Result<(), DocsightError>
    {
        for early_change in [false, true] {
            for grown_width in 10..=12 {
                let mut codes = vec![(256, 9)];
                let mut count = 0;
                for width in 9..grown_width {
                    let new_count = (1 << width) - 257 - usize::from(early_change);
                    codes.extend(std::iter::repeat_n((65, width), new_count - count));
                    count = new_count;
                }
                codes.extend([(256, grown_width), (66, 9), (67, 9), (257, 9)]);
                let mut expected = vec![65; count];
                expected.extend_from_slice(b"BC");
                assert_eq!(
                    lzw_decode(&pack_lzw_codes(&codes), early_change)?,
                    expected,
                    "early_change={early_change}, grown_width={grown_width}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn lzw_clear_discards_previous_dictionary_entries() {
        let codes = [(256, 9), (65, 9), (66, 9), (256, 9), (258, 9), (257, 9)];
        assert!(matches!(
            lzw_decode(&pack_lzw_codes(&codes), true),
            Err(DocsightError::MalformedDocument { .. })
        ));
    }
}
