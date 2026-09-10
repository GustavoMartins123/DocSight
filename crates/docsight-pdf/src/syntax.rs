use docsight_core::DocsightError;
use std::collections::{BTreeMap, BTreeSet};

const MAX_OBJECTS: usize = 100_000;
const MAX_XREF_SECTIONS: usize = 64;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(f32),
    Name(String),
    String(Vec<u8>),
    Array(Vec<Value>),
    Dict(BTreeMap<String, Value>),
    Ref(ObjectRef),
    Stream(StreamValue),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObjectRef {
    pub number: u32,
    pub generation: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StreamValue {
    pub dict: BTreeMap<String, Value>,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum XrefEntry {
    Offset(usize),
    Compressed { stream: u32, index: u32 },
}

pub(crate) struct Xref {
    pub entries: BTreeMap<ObjectRef, XrefEntry>,
    pub trailer: BTreeMap<String, Value>,
}

struct XrefSection {
    entries: BTreeMap<ObjectRef, XrefEntry>,
    trailer: BTreeMap<String, Value>,
    prev: Option<usize>,
    hybrid: Option<usize>,
}

pub(crate) fn parse_xref(bytes: &[u8]) -> Result<Xref, DocsightError> {
    if bytes.len() < 9
        || !bytes.starts_with(b"%PDF-1.")
        || !bytes[7].is_ascii_digit()
        || !matches!(bytes.get(8), Some(b'\r' | b'\n'))
    {
        return Err(malformed("missing PDF header"));
    }
    let eof_marker = b"%%EOF";
    let eof_offset = bytes
        .windows(eof_marker.len())
        .rposition(|window| window == eof_marker)
        .ok_or_else(|| malformed("missing PDF end marker"))?;
    if bytes[eof_offset + eof_marker.len()..]
        .iter()
        .any(|byte| !is_space(*byte))
    {
        return Err(malformed("unexpected bytes after PDF end marker"));
    }
    let marker = b"startxref";
    let marker_offset = bytes[..eof_offset]
        .windows(marker.len())
        .rposition(|window| window == marker)
        .ok_or_else(|| malformed("missing startxref"))?;
    let mut start_parser = Parser::new(bytes, marker_offset + marker.len());
    let xref_offset = start_parser.parse_usize()?;
    if xref_offset >= bytes.len() {
        return Err(malformed("startxref points outside the document"));
    }

    let mut entries: BTreeMap<ObjectRef, XrefEntry> = BTreeMap::new();
    let mut trailer: Option<BTreeMap<String, Value>> = None;
    let mut visited = BTreeSet::new();
    let mut next = Some(xref_offset);
    while let Some(offset) = next {
        if !visited.insert(offset) {
            return Err(malformed("PDF xref chain revisits the same section"));
        }
        if visited.len() > MAX_XREF_SECTIONS {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF xref sections".to_owned(),
                limit: MAX_XREF_SECTIONS as u64,
            });
        }
        let section = parse_xref_section(bytes, offset)?;
        merge_xref_entries(&mut entries, section.entries)?;
        if let Some(hybrid) = section.hybrid
            && visited.insert(hybrid)
        {
            let stream = parse_xref_section(bytes, hybrid)?;
            merge_xref_entries(&mut entries, stream.entries)?;
        }
        if trailer.is_none() {
            trailer = Some(section.trailer);
        }
        next = section.prev;
    }

    let trailer = trailer.ok_or_else(|| malformed("PDF has no trailer"))?;
    match trailer.get("Size") {
        Some(Value::Int(size)) if *size >= 0 && (*size as u64) <= MAX_OBJECTS as u64 => {}
        _ => return Err(malformed("trailer has no valid Size")),
    }
    Ok(Xref { entries, trailer })
}

fn merge_xref_entries(
    into: &mut BTreeMap<ObjectRef, XrefEntry>,
    from: BTreeMap<ObjectRef, XrefEntry>,
) -> Result<(), DocsightError> {
    for (reference, entry) in from {
        if into.len() >= MAX_OBJECTS && !into.contains_key(&reference) {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF xref entries".to_owned(),
                limit: MAX_OBJECTS as u64,
            });
        }
        into.entry(reference).or_insert(entry);
    }
    Ok(())
}

fn parse_xref_section(bytes: &[u8], offset: usize) -> Result<XrefSection, DocsightError> {
    if offset >= bytes.len() {
        return Err(malformed("xref section points outside the document"));
    }
    let mut parser = Parser::new(bytes, offset);
    if parser.consume_keyword(b"xref") {
        return parse_xref_table(parser, bytes);
    }
    parse_xref_stream(bytes, offset)
}

fn parse_xref_table(mut parser: Parser<'_>, bytes: &[u8]) -> Result<XrefSection, DocsightError> {
    let mut entries = BTreeMap::new();
    loop {
        parser.skip_space_and_comments();
        if parser.consume_keyword(b"trailer") {
            break;
        }
        let first = parser.parse_u32()?;
        let count = parser.parse_usize()?;
        let projected = entries.len().checked_add(count).ok_or_else(xref_limit)?;
        if count > MAX_OBJECTS || projected > MAX_OBJECTS {
            return Err(xref_limit());
        }
        for index in 0..count {
            let offset = parser.parse_usize()?;
            let generation = parser.parse_u16()?;
            let status = parser.read_word()?;
            if status == b"n" {
                let index = u32::try_from(index).map_err(|_| xref_limit())?;
                let number = first
                    .checked_add(index)
                    .ok_or_else(|| malformed("xref object number overflow"))?;
                if offset >= bytes.len() {
                    return Err(malformed("xref entry points outside the document"));
                }
                if entries
                    .insert(ObjectRef { number, generation }, XrefEntry::Offset(offset))
                    .is_some()
                {
                    return Err(malformed("duplicate PDF xref entry"));
                }
            } else if status != b"f" {
                return Err(malformed("invalid xref entry status"));
            }
        }
    }
    let trailer = match parser.parse_value(0)? {
        Value::Dict(dict) => dict,
        _ => return Err(malformed("trailer must be a dictionary")),
    };
    let prev = section_offset(&trailer, "Prev", bytes)?;
    let hybrid = section_offset(&trailer, "XRefStm", bytes)?;
    Ok(XrefSection {
        entries,
        trailer,
        prev,
        hybrid,
    })
}

fn parse_xref_stream(bytes: &[u8], offset: usize) -> Result<XrefSection, DocsightError> {
    let mut parser = Parser::new(bytes, offset);
    parser.parse_u32()?;
    parser.parse_u16()?;
    parser.require_keyword(b"obj")?;
    let value = parser.parse_value(0)?;
    let Value::Stream(stream) = value else {
        return Err(malformed(
            "startxref does not point at an xref table or stream",
        ));
    };
    if !matches!(stream.dict.get("Type"), Some(Value::Name(name)) if name == "XRef") {
        return Err(malformed("cross-reference stream is not of type XRef"));
    }
    let data = crate::filters::decode_stream(&stream)?;

    let widths = match stream.dict.get("W") {
        Some(Value::Array(values)) if (1..=8).contains(&values.len()) => values
            .iter()
            .map(|value| match value {
                Value::Int(width) if (0..=8).contains(width) => Ok(*width as usize),
                _ => Err(malformed("xref stream W entry must be a small integer")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(malformed("xref stream has no valid W array")),
    };
    let record_width: usize = widths.iter().sum();
    if record_width == 0 {
        return Err(malformed("xref stream records must not be empty"));
    }

    let size = match stream.dict.get("Size") {
        Some(Value::Int(size)) if *size >= 0 && (*size as u64) <= MAX_OBJECTS as u64 => {
            *size as u32
        }
        _ => return Err(malformed("xref stream has no valid Size")),
    };
    let index: Vec<(u32, usize)> = match stream.dict.get("Index") {
        None => vec![(0, size as usize)],
        Some(Value::Array(values)) if values.len().is_multiple_of(2) => values
            .chunks(2)
            .map(|pair| match (&pair[0], &pair[1]) {
                (Value::Int(start), Value::Int(count))
                    if *start >= 0
                        && *count >= 0
                        && *start <= u32::MAX as i64
                        && (*count as u64) <= MAX_OBJECTS as u64 =>
                {
                    Ok((*start as u32, *count as usize))
                }
                _ => Err(malformed("xref stream Index entry is out of range")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(malformed("xref stream has no valid Index array")),
    };

    let mut entries = BTreeMap::new();
    let mut cursor = 0_usize;
    for (start, count) in index {
        if count > MAX_OBJECTS || entries.len().saturating_add(count) > MAX_OBJECTS {
            return Err(xref_limit());
        }
        for position in 0..count {
            let end = cursor
                .checked_add(record_width)
                .ok_or_else(|| malformed("xref stream record overflow"))?;
            if end > data.len() {
                return Err(malformed("xref stream is shorter than its Index declares"));
            }
            let record = &data[cursor..end];
            cursor = end;
            let mut fields = [1_u64, 0, 0];
            let mut field_cursor = 0_usize;
            for (slot, width) in widths.iter().enumerate().take(3) {
                if *width == 0 {
                    field_cursor += width;
                    continue;
                }
                let mut value = 0_u64;
                for byte in &record[field_cursor..field_cursor + width] {
                    value = (value << 8) | u64::from(*byte);
                }
                fields[slot] = value;
                field_cursor += width;
            }
            let position = u32::try_from(position).map_err(|_| xref_limit())?;
            let number = match start.checked_add(position) {
                Some(number) => number,
                None => return Err(malformed("xref stream object number overflow")),
            };
            match fields[0] {
                0 => {}
                1 => {
                    let offset = usize::try_from(fields[1])
                        .map_err(|_| malformed("xref stream offset is out of range"))?;
                    if offset >= bytes.len() {
                        return Err(malformed("xref stream entry points outside the document"));
                    }
                    let generation = u16::try_from(fields[2])
                        .map_err(|_| malformed("xref stream generation is out of range"))?;
                    entries.insert(ObjectRef { number, generation }, XrefEntry::Offset(offset));
                }
                2 => {
                    let container = u32::try_from(fields[1])
                        .map_err(|_| malformed("object stream number is out of range"))?;
                    let position = u32::try_from(fields[2])
                        .map_err(|_| malformed("object stream index is out of range"))?;
                    entries.insert(
                        ObjectRef {
                            number,
                            generation: 0,
                        },
                        XrefEntry::Compressed {
                            stream: container,
                            index: position,
                        },
                    );
                }
                other => {
                    return Err(malformed(format!("unknown xref stream entry type {other}")));
                }
            }
        }
    }

    let prev = section_offset(&stream.dict, "Prev", bytes)?;
    Ok(XrefSection {
        entries,
        trailer: stream.dict,
        prev,
        hybrid: None,
    })
}

fn section_offset(
    dict: &BTreeMap<String, Value>,
    key: &str,
    bytes: &[u8],
) -> Result<Option<usize>, DocsightError> {
    match dict.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int(offset)) if *offset >= 0 => {
            let offset = usize::try_from(*offset)
                .map_err(|_| malformed(format!("{key} offset is out of range")))?;
            if offset >= bytes.len() {
                return Err(malformed(format!("{key} points outside the document")));
            }
            Ok(Some(offset))
        }
        _ => Err(malformed(format!("{key} must be a byte offset"))),
    }
}

pub(crate) fn parse_object_stream_index(
    data: &[u8],
    count: usize,
    first: usize,
) -> Result<Vec<(u32, usize)>, DocsightError> {
    if count > MAX_OBJECTS {
        return Err(xref_limit());
    }
    let mut parser = Parser::new(data, 0);
    let mut index = Vec::with_capacity(count);
    for _ in 0..count {
        let number = parser.parse_u32()?;
        let offset = parser.parse_usize()?;
        if parser.cursor > first {
            return Err(malformed("object stream index runs past its First offset"));
        }
        let start = first
            .checked_add(offset)
            .ok_or_else(|| malformed("object stream offset overflow"))?;
        if start >= data.len() {
            return Err(malformed("object stream entry points outside the stream"));
        }
        index.push((number, start));
    }
    Ok(index)
}

pub(crate) fn parse_object_stream_value(
    data: &[u8],
    offset: usize,
) -> Result<Value, DocsightError> {
    let mut parser = Parser::new(data, offset);
    let value = parser.parse_value(0)?;
    if matches!(value, Value::Stream(_)) {
        return Err(malformed("object streams must not contain stream objects"));
    }
    Ok(value)
}

pub(crate) fn parse_object(
    bytes: &[u8],
    offset: usize,
    expected: ObjectRef,
    entries: &BTreeMap<ObjectRef, XrefEntry>,
) -> Result<Value, DocsightError> {
    let mut parser = Parser::with_entries(bytes, offset, entries);
    let number = parser.parse_u32()?;
    let generation = parser.parse_u16()?;
    if number != expected.number || generation != expected.generation {
        return Err(malformed(
            "xref entry does not match indirect object header",
        ));
    }
    parser.require_keyword(b"obj")?;
    let value = parser.parse_value(0)?;
    parser.require_keyword(b"endobj")?;
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    cursor: usize,
    entries: Option<&'a BTreeMap<ObjectRef, XrefEntry>>,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8], cursor: usize) -> Self {
        Self {
            bytes,
            cursor,
            entries: None,
        }
    }

    fn with_entries(
        bytes: &'a [u8],
        cursor: usize,
        entries: &'a BTreeMap<ObjectRef, XrefEntry>,
    ) -> Self {
        Self {
            bytes,
            cursor,
            entries: Some(entries),
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, DocsightError> {
        if depth > MAX_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF object nesting".to_owned(),
                limit: MAX_DEPTH as u64,
            });
        }
        self.skip_space_and_comments();
        match self.peek_byte() {
            Some(b'<') if self.peek_next() == Some(b'<') => self.parse_dict_or_stream(depth),
            Some(b'<') => self.parse_hex_string(),
            Some(b'[') => self.parse_array(depth),
            Some(b'(') => self.parse_literal_string(),
            Some(b'/') => self.parse_name().map(Value::Name),
            Some(b't') if self.consume_keyword(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.consume_keyword(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.consume_keyword(b"null") => Ok(Value::Null),
            Some(b'+') | Some(b'-') | Some(b'.') | Some(b'0'..=b'9') => {
                self.parse_number_or_reference()
            }
            _ => Err(malformed("invalid PDF object value")),
        }
    }

    fn parse_dict_or_stream(&mut self, depth: usize) -> Result<Value, DocsightError> {
        self.cursor += 2;
        let mut dict = BTreeMap::new();
        loop {
            self.skip_space_and_comments();
            if self.peek_byte() == Some(b'>') && self.peek_next() == Some(b'>') {
                self.cursor += 2;
                break;
            }
            let key = self.parse_name()?;
            let value = self.parse_value(depth + 1)?;
            if dict.insert(key, value).is_some() {
                return Err(malformed("duplicate PDF dictionary key"));
            }
        }
        self.skip_space_and_comments();
        if !self.consume_keyword(b"stream") {
            return Ok(Value::Dict(dict));
        }
        self.consume_stream_line_ending()?;
        let length = match dict.get("Length") {
            Some(Value::Int(value)) if *value >= 0 => usize::try_from(*value)
                .map_err(|_| malformed("stream length is outside the supported range"))?,
            Some(Value::Ref(reference)) => self.parse_indirect_length(*reference)?,
            _ => return Err(malformed("stream has no valid direct length")),
        };
        let end = self
            .cursor
            .checked_add(length)
            .ok_or_else(|| malformed("stream length overflow"))?;
        if end > self.bytes.len() {
            return Err(malformed("stream extends beyond the document"));
        }
        let data = self.bytes[self.cursor..end].to_vec();
        self.cursor = end;
        self.skip_line_ending();
        self.require_keyword(b"endstream")?;
        Ok(Value::Stream(StreamValue { dict, data }))
    }

    fn parse_indirect_length(&self, reference: ObjectRef) -> Result<usize, DocsightError> {
        let entries = self
            .entries
            .ok_or_else(|| malformed("indirect stream length cannot be resolved here"))?;
        let offset = match entries.get(&reference).copied() {
            Some(XrefEntry::Offset(offset)) => offset,
            Some(XrefEntry::Compressed { .. }) => {
                return Err(malformed(
                    "stream length object must not live inside an object stream",
                ));
            }
            None => {
                return Err(malformed(format!(
                    "missing xref entry for stream length object {}",
                    reference.number
                )));
            }
        };
        let mut parser = Parser::new(self.bytes, offset);
        let number = parser.parse_u32()?;
        let generation = parser.parse_u16()?;
        if number != reference.number || generation != reference.generation {
            return Err(malformed(
                "stream length xref entry does not match object header",
            ));
        }
        parser.require_keyword(b"obj")?;
        let length = match parser.parse_value(0)? {
            Value::Int(value) if value >= 0 => usize::try_from(value)
                .map_err(|_| malformed("stream length is outside the supported range"))?,
            _ => {
                return Err(malformed(
                    "indirect stream length must resolve to an integer",
                ));
            }
        };
        parser.require_keyword(b"endobj")?;
        Ok(length)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, DocsightError> {
        self.cursor += 1;
        let mut values = Vec::new();
        loop {
            self.skip_space_and_comments();
            if self.peek_byte() == Some(b']') {
                self.cursor += 1;
                break;
            }
            if values.len() >= MAX_OBJECTS {
                return Err(DocsightError::ResourceLimit {
                    resource: "PDF array items".to_owned(),
                    limit: MAX_OBJECTS as u64,
                });
            }
            values.push(self.parse_value(depth + 1)?);
        }
        Ok(Value::Array(values))
    }

    fn parse_number_or_reference(&mut self) -> Result<Value, DocsightError> {
        let first = self.read_number_word()?;
        if let Ok(number) = first.parse::<u32>() {
            let saved = self.cursor;
            if let Ok(generation_word) = self.read_number_word()
                && let Ok(generation) = generation_word.parse::<u16>()
                && self.consume_keyword(b"R")
            {
                return Ok(Value::Ref(ObjectRef { number, generation }));
            }
            self.cursor = saved;
        }
        if first.contains('.') {
            let value = first
                .parse::<f32>()
                .map_err(|_| malformed("invalid real number"))?;
            if !value.is_finite() {
                return Err(malformed("non-finite real number"));
            }
            Ok(Value::Real(value))
        } else {
            first
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| malformed("invalid integer"))
        }
    }

    fn parse_name(&mut self) -> Result<String, DocsightError> {
        self.skip_space_and_comments();
        if self.peek_byte() != Some(b'/') {
            return Err(malformed("expected PDF name"));
        }
        self.cursor += 1;
        let mut bytes = Vec::new();
        while let Some(byte) = self.peek_byte() {
            if is_delimiter_or_space(byte) {
                break;
            }
            if byte == b'#' {
                self.cursor += 1;
                let high = self
                    .take_byte()
                    .ok_or_else(|| malformed("truncated name escape"))?;
                let low = self
                    .take_byte()
                    .ok_or_else(|| malformed("truncated name escape"))?;
                bytes.push(hex_pair(high, low)?);
            } else {
                bytes.push(byte);
                self.cursor += 1;
            }
        }
        String::from_utf8(bytes).map_err(|_| malformed("PDF name is not valid UTF-8"))
    }

    fn parse_literal_string(&mut self) -> Result<Value, DocsightError> {
        self.cursor += 1;
        let mut result = Vec::new();
        let mut depth = 1usize;
        while let Some(byte) = self.take_byte() {
            match byte {
                b'(' => {
                    if depth >= MAX_DEPTH {
                        return Err(DocsightError::ResourceLimit {
                            resource: "PDF string nesting".to_owned(),
                            limit: MAX_DEPTH as u64,
                        });
                    }
                    depth += 1;
                    result.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(Value::String(result));
                    }
                    result.push(byte);
                }
                b'\\' => self.parse_string_escape(&mut result)?,
                _ => result.push(byte),
            }
        }
        Err(malformed("unterminated PDF string"))
    }

    fn parse_string_escape(&mut self, result: &mut Vec<u8>) -> Result<(), DocsightError> {
        let byte = self
            .take_byte()
            .ok_or_else(|| malformed("truncated PDF string escape"))?;
        match byte {
            b'n' => result.push(b'\n'),
            b'r' => result.push(b'\r'),
            b't' => result.push(b'\t'),
            b'b' => result.push(8),
            b'f' => result.push(12),
            b'(' | b')' | b'\\' => result.push(byte),
            b'\r' => {
                if self.peek_byte() == Some(b'\n') {
                    self.cursor += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    match self.peek_byte() {
                        Some(next @ b'0'..=b'7') => {
                            self.cursor += 1;
                            value = value * 8 + u16::from(next - b'0');
                        }
                        _ => break,
                    }
                }
                result.push(u8::try_from(value).map_err(|_| malformed("invalid octal escape"))?);
            }
            _ => result.push(byte),
        }
        Ok(())
    }

    fn parse_hex_string(&mut self) -> Result<Value, DocsightError> {
        self.cursor += 1;
        let mut nibbles = Vec::new();
        loop {
            let byte = self
                .take_byte()
                .ok_or_else(|| malformed("unterminated hexadecimal string"))?;
            if byte == b'>' {
                break;
            }
            if !is_space(byte) {
                nibbles.push(hex_digit(byte)?);
            }
        }
        if nibbles.len() % 2 == 1 {
            nibbles.push(0);
        }
        let bytes = nibbles
            .chunks_exact(2)
            .map(|pair| pair[0] * 16 + pair[1])
            .collect();
        Ok(Value::String(bytes))
    }

    fn parse_u32(&mut self) -> Result<u32, DocsightError> {
        self.read_word()?.iter().try_fold(0u32, |value, byte| {
            if !byte.is_ascii_digit() {
                return Err(malformed("expected unsigned integer"));
            }
            value
                .checked_mul(10)
                .and_then(|current| current.checked_add(u32::from(*byte - b'0')))
                .ok_or_else(|| malformed("unsigned integer overflow"))
        })
    }

    fn parse_u16(&mut self) -> Result<u16, DocsightError> {
        let value = self.parse_u32()?;
        u16::try_from(value).map_err(|_| malformed("generation number overflow"))
    }

    fn parse_usize(&mut self) -> Result<usize, DocsightError> {
        self.read_word()?.iter().try_fold(0usize, |value, byte| {
            if !byte.is_ascii_digit() {
                return Err(malformed("expected unsigned integer"));
            }
            value
                .checked_mul(10)
                .and_then(|current| current.checked_add(usize::from(*byte - b'0')))
                .ok_or_else(|| malformed("unsigned integer overflow"))
        })
    }

    fn read_number_word(&mut self) -> Result<String, DocsightError> {
        let word = self.read_word()?;
        String::from_utf8(word.to_vec()).map_err(|_| malformed("number is not ASCII"))
    }

    fn read_word(&mut self) -> Result<&'a [u8], DocsightError> {
        self.skip_space_and_comments();
        let start = self.cursor;
        while let Some(byte) = self.peek_byte() {
            if is_delimiter_or_space(byte) {
                break;
            }
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(malformed("expected PDF token"));
        }
        Ok(&self.bytes[start..self.cursor])
    }

    fn require_keyword(&mut self, keyword: &[u8]) -> Result<(), DocsightError> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            Err(malformed("missing required PDF keyword"))
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> bool {
        self.skip_space_and_comments();
        let end = match self.cursor.checked_add(keyword.len()) {
            Some(end) => end,
            None => return false,
        };
        if self.bytes.get(self.cursor..end) != Some(keyword) {
            return false;
        }
        if self
            .bytes
            .get(end)
            .is_some_and(|byte| !is_delimiter_or_space(*byte))
        {
            return false;
        }
        self.cursor = end;
        true
    }

    fn consume_stream_line_ending(&mut self) -> Result<(), DocsightError> {
        match (self.take_byte(), self.peek_byte()) {
            (Some(b'\r'), Some(b'\n')) => {
                self.cursor += 1;
                Ok(())
            }
            (Some(b'\r' | b'\n'), _) => Ok(()),
            _ => Err(malformed(
                "stream keyword must be followed by a line ending",
            )),
        }
    }

    fn skip_line_ending(&mut self) {
        if self.peek_byte() == Some(b'\r') {
            self.cursor += 1;
            if self.peek_byte() == Some(b'\n') {
                self.cursor += 1;
            }
        } else if self.peek_byte() == Some(b'\n') {
            self.cursor += 1;
        }
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.peek_byte().is_some_and(is_space) {
                self.cursor += 1;
            }
            if self.peek_byte() != Some(b'%') {
                break;
            }
            while let Some(byte) = self.take_byte() {
                if byte == b'\r' || byte == b'\n' {
                    break;
                }
            }
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    fn peek_next(&self) -> Option<u8> {
        self.bytes.get(self.cursor + 1).copied()
    }

    fn take_byte(&mut self) -> Option<u8> {
        let byte = self.peek_byte()?;
        self.cursor += 1;
        Some(byte)
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter_or_space(byte: u8) -> bool {
    is_space(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn hex_pair(high: u8, low: u8) -> Result<u8, DocsightError> {
    Ok(hex_digit(high)? * 16 + hex_digit(low)?)
}

fn hex_digit(byte: u8) -> Result<u8, DocsightError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(malformed("invalid hexadecimal digit")),
    }
}

pub(crate) fn malformed(message: impl Into<String>) -> DocsightError {
    DocsightError::MalformedDocument {
        message: message.into(),
    }
}

fn xref_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "PDF xref entries".to_owned(),
        limit: MAX_OBJECTS as u64,
    }
}
