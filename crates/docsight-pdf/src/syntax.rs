use docsight_core::DocsightError;
use std::collections::BTreeMap;

const MAX_OBJECTS: usize = 100_000;
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

pub(crate) struct Xref {
    pub entries: BTreeMap<ObjectRef, usize>,
    pub trailer: BTreeMap<String, Value>,
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
    let mut parser = Parser::new(bytes, xref_offset);
    if !parser.consume_keyword(b"xref") {
        return Err(DocsightError::UnsupportedFeature {
            feature: "xref streams".to_owned(),
        });
    }
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
            return Err(DocsightError::ResourceLimit {
                resource: "PDF xref entries".to_owned(),
                limit: MAX_OBJECTS as u64,
            });
        }
        for index in 0..count {
            let offset = parser.parse_usize()?;
            let generation = parser.parse_u16()?;
            let status = parser.read_word()?;
            if status == b"n" {
                let index = u32::try_from(index).map_err(|_| DocsightError::ResourceLimit {
                    resource: "PDF xref entries".to_owned(),
                    limit: MAX_OBJECTS as u64,
                })?;
                let number = first
                    .checked_add(index)
                    .ok_or_else(|| malformed("xref object number overflow"))?;
                if offset >= bytes.len() {
                    return Err(malformed("xref entry points outside the document"));
                }
                if entries
                    .insert(ObjectRef { number, generation }, offset)
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
    if trailer.contains_key("Prev") {
        return Err(DocsightError::UnsupportedFeature {
            feature: "incremental PDF updates".to_owned(),
        });
    }
    if trailer.contains_key("XRefStm") {
        return Err(DocsightError::UnsupportedFeature {
            feature: "hybrid xref streams".to_owned(),
        });
    }
    match trailer.get("Size") {
        Some(Value::Int(size)) if *size >= 0 && (*size as u64) <= MAX_OBJECTS as u64 => {}
        _ => return Err(malformed("trailer has no valid Size")),
    }
    Ok(Xref { entries, trailer })
}

pub(crate) fn parse_object(
    bytes: &[u8],
    offset: usize,
    expected: ObjectRef,
    entries: &BTreeMap<ObjectRef, usize>,
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
    entries: Option<&'a BTreeMap<ObjectRef, usize>>,
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
        entries: &'a BTreeMap<ObjectRef, usize>,
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
        let offset = entries.get(&reference).copied().ok_or_else(|| {
            malformed(format!(
                "missing xref entry for stream length object {}",
                reference.number
            ))
        })?;
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
