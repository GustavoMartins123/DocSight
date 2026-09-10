use docsight_core::DocsightError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_FONT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CMAP_ENTRIES: usize = 1_000_000;
const MAX_GLYPH_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FontPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlyphOutline {
    pub contours: Vec<Vec<FontPoint>>,
    pub advance: f32,
    pub units_per_em: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct FontProgram {
    data: Vec<u8>,
    sha256: String,
    units_per_em: u16,
    glyph_count: u16,
    hmetrics: u16,
    loca: Vec<u32>,
    glyf_offset: usize,
    hmtx_offset: usize,
    cmap: BTreeMap<u32, u16>,
}

impl FontProgram {
    pub(crate) fn parse(data: Vec<u8>) -> Result<Self, DocsightError> {
        if data.len() > MAX_FONT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "embedded TrueType font bytes".to_owned(),
                limit: MAX_FONT_BYTES as u64,
            });
        }
        let tables = tables(&data)?;
        let (head, _) = required_table(&tables, b"head")?;
        let units_per_em = u16_at(&data, head + 18)?;
        if units_per_em == 0 {
            return Err(malformed("TrueType units per em must be non-zero"));
        }
        let index_to_loc_format = i16_at(&data, head + 50)?;
        if !matches!(index_to_loc_format, 0 | 1) {
            return Err(malformed("unsupported TrueType loca format"));
        }
        let (maxp, _) = required_table(&tables, b"maxp")?;
        let glyph_count = u16_at(&data, maxp + 4)?;
        if glyph_count == 0 {
            return Err(malformed("TrueType font has no glyphs"));
        }
        let (hhea, _) = required_table(&tables, b"hhea")?;
        let hmetrics = u16_at(&data, hhea + 34)?;
        if hmetrics == 0 || hmetrics > glyph_count {
            return Err(malformed("invalid TrueType horizontal metrics count"));
        }
        let (hmtx, hmtx_len) = required_table(&tables, b"hmtx")?;
        let minimum_hmtx = usize::from(hmetrics)
            .checked_mul(4)
            .ok_or_else(|| malformed("TrueType horizontal metrics overflow"))?;
        if minimum_hmtx > hmtx_len {
            return Err(malformed("TrueType horizontal metrics table is truncated"));
        }
        let (glyf, glyf_len) = required_table(&tables, b"glyf")?;
        let (loca_offset, loca_len) = required_table(&tables, b"loca")?;
        let loca_item_size = if index_to_loc_format == 0 { 2 } else { 4 };
        let loca_count = usize::from(glyph_count)
            .checked_add(1)
            .ok_or_else(|| malformed("TrueType loca count overflow"))?;
        if loca_count
            .checked_mul(loca_item_size)
            .ok_or_else(|| malformed("TrueType loca size overflow"))?
            > loca_len
        {
            return Err(malformed("TrueType loca table is truncated"));
        }
        let mut loca = Vec::with_capacity(loca_count);
        for index in 0..loca_count {
            let offset = loca_offset
                .checked_add(
                    index
                        .checked_mul(loca_item_size)
                        .ok_or_else(|| malformed("TrueType loca offset overflow"))?,
                )
                .ok_or_else(|| malformed("TrueType loca offset overflow"))?;
            let value = if index_to_loc_format == 0 {
                u32::from(u16_at(&data, offset)?) * 2
            } else {
                u32_at(&data, offset)?
            };
            if value as usize > glyf_len {
                return Err(malformed("TrueType loca entry exceeds glyf table"));
            }
            if let Some(previous) = loca.last()
                && value < *previous
            {
                return Err(malformed("TrueType loca entries are not ordered"));
            }
            loca.push(value);
        }
        let cmap = match tables.get(b"cmap") {
            Some(_) => parse_cmap(&data, &tables)?,
            None => BTreeMap::new(),
        };
        let sha256 = Sha256::digest(&data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            data,
            sha256,
            units_per_em,
            glyph_count,
            hmetrics,
            loca,
            glyf_offset: glyf,
            hmtx_offset: hmtx,
            cmap,
        })
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn glyph_for_char(&self, character: char) -> Result<u16, DocsightError> {
        self.cmap.get(&(character as u32)).copied().ok_or_else(|| {
            DocsightError::UnsupportedFeature {
                feature: format!("embedded TrueType cmap has no character {character:?}"),
            }
        })
    }

    pub(crate) fn glyph(&self, glyph_id: u16) -> Result<GlyphOutline, DocsightError> {
        if glyph_id >= self.glyph_count {
            return Err(DocsightError::MalformedDocument {
                message: format!("TrueType glyph id {glyph_id} exceeds glyph count"),
            });
        }
        let advance = self.advance(glyph_id)?;
        let index = usize::from(glyph_id);
        let start = self.loca[index] as usize;
        let end = self.loca[index + 1] as usize;
        let glyph = self
            .data
            .get(self.glyf_offset + start..self.glyf_offset + end)
            .ok_or_else(|| malformed("TrueType glyph data is outside glyf table"))?;
        let contours = self.contours(glyph, 0)?;
        Ok(GlyphOutline {
            contours,
            advance,
            units_per_em: f32::from(self.units_per_em),
        })
    }

    pub(crate) fn glyphs_for_identity(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<GlyphOutline>, DocsightError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(malformed(
                "Identity-H text requires two-byte character codes",
            ));
        }
        bytes
            .chunks_exact(2)
            .map(|code| self.glyph(u16::from_be_bytes([code[0], code[1]])))
            .collect()
    }

    pub(crate) fn glyphs_for_text(&self, text: &str) -> Result<Vec<GlyphOutline>, DocsightError> {
        text.chars()
            .map(|character| self.glyph(self.glyph_for_char(character)?))
            .collect()
    }

    fn advance(&self, glyph_id: u16) -> Result<f32, DocsightError> {
        let metric = usize::from(glyph_id.min(self.hmetrics - 1));
        let offset = self
            .hmtx_offset
            .checked_add(
                metric
                    .checked_mul(4)
                    .ok_or_else(|| malformed("TrueType metric offset overflow"))?,
            )
            .ok_or_else(|| malformed("TrueType metric offset overflow"))?;
        Ok(f32::from(u16_at(&self.data, offset)?))
    }

    fn contours(&self, glyph: &[u8], depth: usize) -> Result<Vec<Vec<FontPoint>>, DocsightError> {
        if glyph.is_empty() {
            return Ok(Vec::new());
        }
        if depth > MAX_GLYPH_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "TrueType composite glyph depth".to_owned(),
                limit: MAX_GLYPH_DEPTH as u64,
            });
        }
        let number_of_contours = i16_at(glyph, 0)?;
        if number_of_contours >= 0 {
            return simple_contours(
                glyph,
                usize::try_from(number_of_contours)
                    .map_err(|_| malformed("TrueType contour count overflow"))?,
            );
        }
        composite_contours(self, glyph, depth + 1)
    }
}

#[derive(Clone, Copy)]
struct Table {
    offset: usize,
    length: usize,
}

fn tables(data: &[u8]) -> Result<BTreeMap<[u8; 4], Table>, DocsightError> {
    if data.len() < 12 || &data[0..4] != b"\x00\x01\x00\x00" {
        return Err(DocsightError::UnsupportedFeature {
            feature: "embedded font is not a TrueType sfnt".to_owned(),
        });
    }
    let count = usize::from(u16_at(data, 4)?);
    let directory_len = count
        .checked_mul(16)
        .and_then(|value| value.checked_add(12))
        .ok_or_else(|| malformed("TrueType table directory overflow"))?;
    if directory_len > data.len() {
        return Err(malformed("TrueType table directory is truncated"));
    }
    let mut result = BTreeMap::new();
    for index in 0..count {
        let offset = 12 + index * 16;
        let tag = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        let table_offset = usize::try_from(u32_at(data, offset + 8)?)
            .map_err(|_| malformed("TrueType table offset overflow"))?;
        let length = usize::try_from(u32_at(data, offset + 12)?)
            .map_err(|_| malformed("TrueType table length overflow"))?;
        let end = table_offset
            .checked_add(length)
            .ok_or_else(|| malformed("TrueType table bounds overflow"))?;
        if end > data.len() {
            return Err(malformed("TrueType table exceeds embedded font"));
        }
        if result
            .insert(
                tag,
                Table {
                    offset: table_offset,
                    length,
                },
            )
            .is_some()
        {
            return Err(malformed(
                "TrueType table directory contains duplicate tags",
            ));
        }
    }
    Ok(result)
}

fn required_table(
    tables: &BTreeMap<[u8; 4], Table>,
    tag: &[u8; 4],
) -> Result<(usize, usize), DocsightError> {
    let table = tables.get(tag).ok_or_else(|| {
        malformed(format!(
            "embedded TrueType font has no {} table",
            String::from_utf8_lossy(tag)
        ))
    })?;
    Ok((table.offset, table.length))
}

fn parse_cmap(
    data: &[u8],
    tables: &BTreeMap<[u8; 4], Table>,
) -> Result<BTreeMap<u32, u16>, DocsightError> {
    let (offset, length) = required_table(tables, b"cmap")?;
    if length < 4 {
        return Err(malformed("TrueType cmap table is truncated"));
    }
    let records = usize::from(u16_at(data, offset + 2)?);
    let record_end = offset
        .checked_add(4)
        .and_then(|value| value.checked_add(records.checked_mul(8)?))
        .ok_or_else(|| malformed("TrueType cmap records overflow"))?;
    if record_end > offset + length {
        return Err(malformed("TrueType cmap records are truncated"));
    }
    let mut selected = None;
    for index in 0..records {
        let record = offset + 4 + index * 8;
        let platform = u16_at(data, record)?;
        let encoding = u16_at(data, record + 2)?;
        let subtable_offset = usize::try_from(u32_at(data, record + 4)?)
            .map_err(|_| malformed("TrueType cmap offset overflow"))?;
        let absolute = offset
            .checked_add(subtable_offset)
            .ok_or_else(|| malformed("TrueType cmap offset overflow"))?;
        let format = u16_at(data, absolute)?;
        let rank = match (platform, encoding, format) {
            (3, 10, 12) => 0,
            (0, _, 12) => 1,
            (3, 1, 4) => 2,
            (0, _, 4) => 3,
            _ => continue,
        };
        if selected.map(|(old, _)| rank < old).unwrap_or(true) {
            selected = Some((rank, absolute));
        }
    }
    let (_, offset) = selected.ok_or_else(|| DocsightError::UnsupportedFeature {
        feature: "embedded TrueType font has no supported cmap".to_owned(),
    })?;
    match u16_at(data, offset)? {
        4 => parse_cmap4(data, offset),
        12 => parse_cmap12(data, offset),
        _ => Err(DocsightError::UnsupportedFeature {
            feature: "embedded TrueType cmap format".to_owned(),
        }),
    }
}

fn parse_cmap4(data: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, DocsightError> {
    let length = usize::from(u16_at(data, offset + 2)?);
    let end = offset
        .checked_add(length)
        .ok_or_else(|| malformed("TrueType cmap format 4 overflow"))?;
    if end > data.len() || length < 16 {
        return Err(malformed("TrueType cmap format 4 is truncated"));
    }
    let segment_count = usize::from(u16_at(data, offset + 6)?) / 2;
    let end_codes = offset + 14;
    let start_codes = end_codes + segment_count * 2 + 2;
    let deltas = start_codes + segment_count * 2;
    let ranges = deltas + segment_count * 2;
    if ranges + segment_count * 2 > end {
        return Err(malformed("TrueType cmap format 4 arrays are truncated"));
    }
    let mut map = BTreeMap::new();
    for index in 0..segment_count {
        let start = u32::from(u16_at(data, start_codes + index * 2)?);
        let finish = u32::from(u16_at(data, end_codes + index * 2)?);
        if finish < start {
            return Err(malformed("TrueType cmap format 4 range is reversed"));
        }
        if finish == 0xffff {
            continue;
        }
        let delta = i32::from(i16_at(data, deltas + index * 2)?);
        let range_offset = u32::from(u16_at(data, ranges + index * 2)?);
        for code in start..=finish {
            let glyph = if range_offset == 0 {
                ((code as i32 + delta) & 0xffff) as u16
            } else {
                let location = ranges
                    .checked_add(index * 2)
                    .and_then(|value| value.checked_add(range_offset as usize))
                    .and_then(|value| value.checked_add((code - start) as usize * 2))
                    .ok_or_else(|| malformed("TrueType cmap glyph offset overflow"))?;
                if location + 2 > end {
                    return Err(malformed("TrueType cmap glyph offset exceeds table"));
                }
                let glyph = u16_at(data, location)?;
                if glyph == 0 {
                    0
                } else {
                    ((i32::from(glyph) + delta) & 0xffff) as u16
                }
            };
            if glyph != 0 {
                if map.len() >= MAX_CMAP_ENTRIES {
                    return Err(DocsightError::ResourceLimit {
                        resource: "TrueType cmap entries".to_owned(),
                        limit: MAX_CMAP_ENTRIES as u64,
                    });
                }
                map.insert(code, glyph);
            }
        }
    }
    Ok(map)
}

fn parse_cmap12(data: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, DocsightError> {
    let length = usize::try_from(u32_at(data, offset + 4)?)
        .map_err(|_| malformed("TrueType cmap format 12 length overflow"))?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| malformed("TrueType cmap format 12 overflow"))?;
    let groups = usize::try_from(u32_at(data, offset + 12)?)
        .map_err(|_| malformed("TrueType cmap group count overflow"))?;
    if end > data.len()
        || 16usize
            .checked_add(
                groups
                    .checked_mul(12)
                    .ok_or_else(|| malformed("TrueType cmap groups overflow"))?,
            )
            .ok_or_else(|| malformed("TrueType cmap groups overflow"))?
            > length
    {
        return Err(malformed("TrueType cmap format 12 is truncated"));
    }
    let mut map = BTreeMap::new();
    for index in 0..groups {
        let item = offset + 16 + index * 12;
        let start = u32_at(data, item)?;
        let finish = u32_at(data, item + 4)?;
        if finish < start || finish - start > MAX_CMAP_ENTRIES as u32 {
            return Err(malformed("TrueType cmap format 12 range is invalid"));
        }
        let glyph_start = u32_at(data, item + 8)?;
        for code in start..=finish {
            if map.len() >= MAX_CMAP_ENTRIES {
                return Err(DocsightError::ResourceLimit {
                    resource: "TrueType cmap entries".to_owned(),
                    limit: MAX_CMAP_ENTRIES as u64,
                });
            }
            let glyph = glyph_start
                .checked_add(code - start)
                .ok_or_else(|| malformed("TrueType cmap glyph id overflow"))?;
            if glyph <= u32::from(u16::MAX) && glyph != 0 {
                map.insert(code, glyph as u16);
            }
        }
    }
    Ok(map)
}

fn simple_contours(
    data: &[u8],
    contour_count: usize,
) -> Result<Vec<Vec<FontPoint>>, DocsightError> {
    if contour_count == 0 {
        return Ok(Vec::new());
    }
    let endpoints_end = 10usize
        .checked_add(
            contour_count
                .checked_mul(2)
                .ok_or_else(|| malformed("TrueType contour endpoints overflow"))?,
        )
        .ok_or_else(|| malformed("TrueType contour endpoints overflow"))?;
    if endpoints_end + 2 > data.len() {
        return Err(malformed("TrueType glyph contour endpoints are truncated"));
    }
    let mut endpoints = Vec::with_capacity(contour_count);
    for index in 0..contour_count {
        endpoints.push(usize::from(u16_at(data, 10 + index * 2)?));
    }
    let point_count = endpoints
        .last()
        .copied()
        .ok_or_else(|| malformed("TrueType glyph has no contour endpoint"))?
        .checked_add(1)
        .ok_or_else(|| malformed("TrueType point count overflow"))?;
    let instruction_length = usize::from(u16_at(data, endpoints_end)?);
    let mut cursor = endpoints_end + 2;
    cursor = cursor
        .checked_add(instruction_length)
        .ok_or_else(|| malformed("TrueType instructions overflow"))?;
    if cursor > data.len() {
        return Err(malformed("TrueType glyph instructions are truncated"));
    }
    let mut flags = Vec::with_capacity(point_count);
    while flags.len() < point_count {
        let flag = *data
            .get(cursor)
            .ok_or_else(|| malformed("TrueType glyph flags are truncated"))?;
        cursor += 1;
        flags.push(flag);
        if flag & 8 != 0 {
            let repeat = usize::from(
                *data
                    .get(cursor)
                    .ok_or_else(|| malformed("TrueType glyph flag repeat is truncated"))?,
            );
            cursor += 1;
            if flags
                .len()
                .checked_add(repeat)
                .ok_or_else(|| malformed("TrueType glyph flag count overflow"))?
                > point_count
            {
                return Err(malformed("TrueType glyph flag repeat exceeds point count"));
            }
            flags.extend(std::iter::repeat_n(flag, repeat));
        }
    }
    let mut x = Vec::with_capacity(point_count);
    let mut current = 0i32;
    for flag in &flags {
        let delta = if flag & 2 != 0 {
            let value = i32::from(
                *data
                    .get(cursor)
                    .ok_or_else(|| malformed("TrueType x coordinate is truncated"))?,
            );
            cursor += 1;
            if flag & 16 != 0 { value } else { -value }
        } else if flag & 16 != 0 {
            0
        } else {
            let value = i16_at(data, cursor)?;
            cursor += 2;
            i32::from(value)
        };
        current = current
            .checked_add(delta)
            .ok_or_else(|| malformed("TrueType x coordinate overflow"))?;
        x.push(current as f32);
    }
    let mut y = Vec::with_capacity(point_count);
    let mut current = 0i32;
    for flag in &flags {
        let delta = if flag & 4 != 0 {
            let value = i32::from(
                *data
                    .get(cursor)
                    .ok_or_else(|| malformed("TrueType y coordinate is truncated"))?,
            );
            cursor += 1;
            if flag & 32 != 0 { value } else { -value }
        } else if flag & 32 != 0 {
            0
        } else {
            let value = i16_at(data, cursor)?;
            cursor += 2;
            i32::from(value)
        };
        current = current
            .checked_add(delta)
            .ok_or_else(|| malformed("TrueType y coordinate overflow"))?;
        y.push(current as f32);
    }
    let mut contours = Vec::with_capacity(contour_count);
    let mut start = 0usize;
    for end in endpoints {
        if end < start || end >= point_count {
            return Err(malformed("TrueType contour endpoint is invalid"));
        }
        let points = (start..=end)
            .map(|index| RawPoint {
                point: FontPoint {
                    x: x[index],
                    y: y[index],
                },
                on_curve: flags[index] & 1 != 0,
            })
            .collect::<Vec<_>>();
        contours.push(contour_polygon(&points));
        start = end + 1;
    }
    Ok(contours)
}

#[derive(Clone, Copy)]
struct RawPoint {
    point: FontPoint,
    on_curve: bool,
}

fn contour_polygon(points: &[RawPoint]) -> Vec<FontPoint> {
    if points.is_empty() {
        return Vec::new();
    }
    let start = if points[0].on_curve {
        points[0].point
    } else if points[points.len() - 1].on_curve {
        points[points.len() - 1].point
    } else {
        midpoint(points[points.len() - 1].point, points[0].point)
    };
    let mut polygon = vec![start];
    let mut current = start;
    let mut index = 0usize;
    while index < points.len() {
        let point = points[index];
        if point.on_curve {
            if point.point != current {
                polygon.push(point.point);
            }
            current = point.point;
            index += 1;
        } else {
            let next = points[(index + 1) % points.len()];
            let end = if next.on_curve {
                index += 2;
                next.point
            } else {
                index += 1;
                midpoint(point.point, next.point)
            };
            for step in 1..=8 {
                let t = step as f32 / 8.0;
                polygon.push(quadratic(current, point.point, end, t));
            }
            current = end;
        }
    }
    if polygon.last().copied() != Some(start) {
        polygon.push(start);
    }
    polygon
}

fn composite_contours(
    font: &FontProgram,
    data: &[u8],
    depth: usize,
) -> Result<Vec<Vec<FontPoint>>, DocsightError> {
    let mut cursor = 10usize;
    let mut contours = Vec::new();
    let mut more = true;
    while more {
        let flags = u16_at(data, cursor)?;
        cursor += 2;
        let glyph = u16_at(data, cursor)?;
        cursor += 2;
        let (arg_x, arg_y) = if flags & 1 != 0 {
            let x = i16_at(data, cursor)?;
            let y = i16_at(data, cursor + 2)?;
            cursor += 4;
            (x, y)
        } else {
            let x = i8::from_ne_bytes([*data
                .get(cursor)
                .ok_or_else(|| malformed("TrueType composite arguments are truncated"))?])
                as i16;
            let y = i8::from_ne_bytes([*data
                .get(cursor + 1)
                .ok_or_else(|| malformed("TrueType composite arguments are truncated"))?])
                as i16;
            cursor += 2;
            (x, y)
        };
        if flags & 2 == 0 {
            return Err(DocsightError::UnsupportedFeature {
                feature: "TrueType composite point attachment".to_owned(),
            });
        }
        let (a, b, c, d) = if flags & 0x80 != 0 {
            let a = i16_at(data, cursor)? as f32 / 16384.0;
            let b = i16_at(data, cursor + 2)? as f32 / 16384.0;
            let c = i16_at(data, cursor + 4)? as f32 / 16384.0;
            let d = i16_at(data, cursor + 6)? as f32 / 16384.0;
            cursor += 8;
            (a, b, c, d)
        } else if flags & 0x40 != 0 {
            let a = i16_at(data, cursor)? as f32 / 16384.0;
            let d = i16_at(data, cursor + 2)? as f32 / 16384.0;
            cursor += 4;
            (a, 0.0, 0.0, d)
        } else if flags & 0x08 != 0 {
            let a = i16_at(data, cursor)? as f32 / 16384.0;
            cursor += 2;
            (a, 0.0, 0.0, a)
        } else {
            (1.0, 0.0, 0.0, 1.0)
        };
        let child_index = usize::from(glyph);
        let child_start = *font
            .loca
            .get(child_index)
            .ok_or_else(|| malformed("TrueType composite glyph id exceeds loca table"))?
            as usize;
        let child_end = *font
            .loca
            .get(child_index + 1)
            .ok_or_else(|| malformed("TrueType composite glyph id exceeds loca table"))?
            as usize;
        let child = font
            .data
            .get(font.glyf_offset + child_start..font.glyf_offset + child_end)
            .ok_or_else(|| malformed("TrueType composite glyph is outside glyf table"))?;
        for polygon in font.contours(child, depth)? {
            contours.push(
                polygon
                    .into_iter()
                    .map(|point| FontPoint {
                        x: a * point.x + c * point.y + f32::from(arg_x),
                        y: b * point.x + d * point.y + f32::from(arg_y),
                    })
                    .collect(),
            );
        }
        more = flags & 0x20 != 0;
        if !more && flags & 0x100 != 0 {
            let length = usize::from(u16_at(data, cursor)?);
            cursor += 2 + length;
        }
    }
    if cursor > data.len() {
        return Err(malformed("TrueType composite glyph is truncated"));
    }
    Ok(contours)
}

fn midpoint(first: FontPoint, second: FontPoint) -> FontPoint {
    FontPoint {
        x: (first.x + second.x) * 0.5,
        y: (first.y + second.y) * 0.5,
    }
}
fn quadratic(start: FontPoint, control: FontPoint, end: FontPoint, t: f32) -> FontPoint {
    let inverse = 1.0 - t;
    FontPoint {
        x: inverse * inverse * start.x + 2.0 * inverse * t * control.x + t * t * end.x,
        y: inverse * inverse * start.y + 2.0 * inverse * t * control.y + t * t * end.y,
    }
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, DocsightError> {
    let bytes = data
        .get(
            offset
                ..offset
                    .checked_add(2)
                    .ok_or_else(|| malformed("TrueType offset overflow"))?,
        )
        .ok_or_else(|| malformed("TrueType table is truncated"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn i16_at(data: &[u8], offset: usize) -> Result<i16, DocsightError> {
    Ok(i16::from_be_bytes(u16_at(data, offset)?.to_be_bytes()))
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32, DocsightError> {
    let bytes = data
        .get(
            offset
                ..offset
                    .checked_add(4)
                    .ok_or_else(|| malformed("TrueType offset overflow"))?,
        )
        .ok_or_else(|| malformed("TrueType table is truncated"))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
fn malformed(message: impl Into<String>) -> DocsightError {
    DocsightError::MalformedDocument {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::FontProgram;
    use docsight_core::DocsightError;

    #[test]
    fn rejects_non_truetype_programs() {
        assert!(matches!(
            FontProgram::parse(Vec::new()),
            Err(DocsightError::UnsupportedFeature { feature }) if feature.contains("TrueType")
        ));
    }

    #[test]
    fn rejects_truncated_table_directory() {
        let mut bytes = b"\x00\x01\x00\x00".to_vec();
        bytes.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            FontProgram::parse(bytes),
            Err(DocsightError::MalformedDocument { .. })
        ));
    }
}
