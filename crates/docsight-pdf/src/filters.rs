use crate::syntax::{StreamValue, Value, malformed};
use docsight_core::DocsightError;
use std::collections::BTreeMap;
use std::io::{Read, Take};

const MAX_FLATE_OUTPUT_BYTES: u64 = docsight_core::MAX_INSPECT_BYTES;
const MAX_PREDICTOR_COLUMNS: usize = 1 << 20;

pub(crate) fn decode_stream(stream: &StreamValue) -> Result<Vec<u8>, DocsightError> {
    decode_filtered(&stream.dict, &stream.data, &indirect_unsupported)
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
        output = match name.as_str() {
            "FlateDecode" | "Fl" => inflate_zlib(&output)?,
            "ASCII85Decode" | "A85" => ascii85_decode(&output)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&output)?,
            other => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF stream filter {other}"),
                });
            }
        };
        let parm = match parm {
            Value::Ref(_) => resolve(&parm)?,
            other => other,
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
