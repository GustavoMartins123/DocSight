use crate::content::{glyph_name_character, named_encoding_character};
use crate::font::{FontPoint, GlyphOutline, MAX_FONT_BYTES};
use crate::syntax::Value;
use docsight_core::DocsightError;
use hayro_font::{GlyphId, Matrix, OutlineBuilder};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub(crate) const MAX_POINTS: usize = 131_072;
const MAX_CACHE: usize = 4096;

#[derive(Clone, Debug)]
enum Program {
    Type1(hayro_font::type1::Table),
    Cff,
}

#[derive(Clone, Debug)]
pub(crate) struct PostScriptProgram {
    data: Vec<u8>,
    program: Program,
    matrix: Matrix,
    encoding: BTreeMap<u8, String>,
    widths: Option<[f32; 256]>,
    digest: String,
    cache: RefCell<BTreeMap<u16, GlyphOutline>>,
}

fn malformed(message: impl Into<String>) -> DocsightError {
    DocsightError::MalformedDocument {
        message: message.into(),
    }
}

impl PostScriptProgram {
    pub(crate) fn parse(
        data: Vec<u8>,
        cff: bool,
        encoding: Option<&Value>,
        widths: Option<[f32; 256]>,
    ) -> Result<Self, DocsightError> {
        if data.len() > MAX_FONT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "embedded PostScript font bytes".to_owned(),
                limit: MAX_FONT_BYTES as u64,
            });
        }
        let (program, matrix, mut names) = if cff {
            let table = hayro_font::cff::Table::parse(&data)
                .ok_or_else(|| malformed("invalid embedded CFF font"))?;
            let names = (0..=u8::MAX)
                .filter_map(|code| {
                    let glyph = table.glyph_index(code)?;
                    Some((code, table.glyph_name(glyph)?.to_owned()))
                })
                .collect();
            (Program::Cff, table.matrix(), names)
        } else {
            let table = hayro_font::type1::Table::parse(&data)
                .ok_or_else(|| malformed("invalid embedded Type1 font"))?;
            let names = (0..=u8::MAX)
                .filter_map(|code| {
                    table
                        .code_to_string(code)
                        .map(|name| (code, name.to_owned()))
                })
                .collect();
            let matrix = table.matrix();
            (Program::Type1(table), matrix, names)
        };
        if ![
            matrix.sx, matrix.sy, matrix.kx, matrix.ky, matrix.tx, matrix.ty,
        ]
        .iter()
        .all(|value| value.is_finite())
            || matrix.sx * matrix.sy - matrix.kx * matrix.ky == 0.0
        {
            return Err(malformed("embedded PostScript font matrix is invalid"));
        }
        apply_encoding(&mut names, encoding)?;
        let digest = Sha256::digest(&data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            data,
            program,
            matrix,
            encoding: names,
            widths,
            digest,
            cache: RefCell::new(BTreeMap::new()),
        })
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.digest
    }

    pub(crate) fn cache_size(&self) -> usize {
        self.data
            .len()
            .saturating_mul(4)
            .saturating_add(self.encoding.values().map(String::len).sum::<usize>())
    }

    pub(crate) fn characters(&self) -> [Option<char>; 256] {
        let mut characters = [None; 256];
        for (code, name) in &self.encoding {
            characters[usize::from(*code)] = glyph_name_character(name);
        }
        characters
    }

    pub(crate) fn simple(&self, bytes: &[u8]) -> Result<Vec<GlyphOutline>, DocsightError> {
        bytes
            .iter()
            .map(|code| self.glyph(u16::from(*code), false))
            .collect()
    }

    pub(crate) fn identity(
        &self,
        bytes: &[u8],
        table: Option<&[u16]>,
    ) -> Result<Vec<GlyphOutline>, DocsightError> {
        if !bytes.len().is_multiple_of(2) || table.is_some() {
            return Err(malformed(
                "CFF CID text requires two-byte codes without a TrueType CIDToGIDMap",
            ));
        }
        bytes
            .chunks_exact(2)
            .map(|code| self.glyph(u16::from_be_bytes([code[0], code[1]]), true))
            .collect()
    }

    fn glyph(&self, code: u16, cid: bool) -> Result<GlyphOutline, DocsightError> {
        if let Some(outline) = self.cache.borrow().get(&code).cloned() {
            return Ok(outline);
        }
        let mut builder = Contours::new(self.matrix);
        let advance = match &self.program {
            Program::Type1(table) => {
                if cid {
                    return Err(malformed("Type1 font cannot use CID text"));
                }
                let code =
                    u8::try_from(code).map_err(|_| malformed("Type1 code exceeds one byte"))?;
                let name = self.encoding.get(&code).ok_or_else(|| {
                    malformed(format!(
                        "embedded Type1 font has no glyph name for code {code}"
                    ))
                })?;
                table.outline(name, &mut builder).ok_or_else(|| {
                    malformed(format!("embedded Type1 glyph {name} cannot be outlined"))
                })?;
                self.widths
                    .as_ref()
                    .ok_or_else(|| DocsightError::UnsupportedFeature {
                        feature: "embedded Type1 font without declared PDF Widths".to_owned(),
                    })?[usize::from(code)]
            }
            Program::Cff => {
                let table = hayro_font::cff::Table::parse(&self.data)
                    .ok_or_else(|| malformed("validated CFF font cannot be reopened"))?;
                let glyph = if cid {
                    table.glyph_index_by_cid(code)
                } else {
                    let code = u8::try_from(code)
                        .map_err(|_| malformed("CFF simple code exceeds one byte"))?;
                    self.encoding
                        .get(&code)
                        .and_then(|name| table.glyph_index_by_name(name))
                }
                .ok_or_else(|| {
                    malformed(format!("embedded CFF font has no glyph for code {code}"))
                })?;
                match table.outline(glyph, &mut builder) {
                    Ok(_) | Err(hayro_font::OutlineError::ZeroBBox) => {}
                    Err(error) => {
                        return Err(malformed(format!(
                            "embedded CFF glyph {} failed: {error:?}",
                            glyph.0
                        )));
                    }
                }
                f32::from(
                    table
                        .glyph_width(GlyphId(glyph.0))
                        .ok_or_else(|| malformed("embedded CFF glyph has no advance"))?,
                )
            }
        };
        let outline = GlyphOutline {
            contours: builder.finish()?,
            advance,
            units_per_em: 1000.0,
        };
        if self.cache.borrow().len() < MAX_CACHE {
            self.cache.borrow_mut().insert(code, outline.clone());
        }
        Ok(outline)
    }
}

fn apply_encoding(
    names: &mut BTreeMap<u8, String>,
    encoding: Option<&Value>,
) -> Result<(), DocsightError> {
    let (base, differences) = match encoding {
        None => (None, None),
        Some(Value::Name(name)) => (Some(name.as_str()), None),
        Some(Value::Dict(dict)) => {
            let base = match dict.get("BaseEncoding") {
                None => None,
                Some(Value::Name(name)) => Some(name.as_str()),
                _ => return Err(malformed("PostScript BaseEncoding must be a name")),
            };
            (base, dict.get("Differences"))
        }
        _ => {
            return Err(malformed(
                "PostScript Encoding must be a name or dictionary",
            ));
        }
    };
    if let Some(base) = base {
        names.clear();
        for code in 0..=u8::MAX {
            if let Some(character) = named_encoding_character(base, code)?
                && let Some(name) = crate::content::character_glyph_name(character)
            {
                names.insert(code, name.to_owned());
            }
        }
    }
    if let Some(differences) = differences {
        let Value::Array(items) = differences else {
            return Err(malformed("PostScript Differences must be an array"));
        };
        let mut code = None;
        for item in items {
            match item {
                Value::Int(start) => {
                    code = Some(u16::from(u8::try_from(*start).map_err(|_| {
                        malformed("PostScript Differences code exceeds one byte")
                    })?))
                }
                Value::Name(name) => {
                    let current = code.ok_or_else(|| {
                        malformed("PostScript Differences requires an initial code")
                    })?;
                    names.insert(
                        u8::try_from(current).map_err(|_| {
                            malformed("PostScript Differences range exceeds one byte")
                        })?,
                        name.clone(),
                    );
                    code = Some(current + 1);
                }
                _ => {
                    return Err(malformed(
                        "PostScript Differences requires codes and glyph names",
                    ));
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum OutlineFailure {
    InvalidCoordinate,
    CoordinateLimit,
    PointLimit,
}

struct Contours {
    matrix: Matrix,
    contours: Vec<Vec<FontPoint>>,
    current: Vec<FontPoint>,
    point: FontPoint,
    points: usize,
    failure: Option<OutlineFailure>,
}

impl Contours {
    fn new(matrix: Matrix) -> Self {
        Self {
            matrix,
            contours: Vec::new(),
            current: Vec::new(),
            point: FontPoint { x: 0.0, y: 0.0 },
            points: 0,
            failure: None,
        }
    }
    fn push(&mut self, x: f32, y: f32) {
        self.point = FontPoint { x, y };
        let m = self.matrix;
        let point = FontPoint {
            x: (m.sx * x + m.kx * y + m.tx) * 1000.0,
            y: (m.ky * x + m.sy * y + m.ty) * 1000.0,
        };
        if self.failure.is_some() {
            return;
        }
        self.failure = if !point.x.is_finite() || !point.y.is_finite() {
            Some(OutlineFailure::InvalidCoordinate)
        } else if point.x.abs() > 10_000_000.0 || point.y.abs() > 10_000_000.0 {
            Some(OutlineFailure::CoordinateLimit)
        } else if self.points >= MAX_POINTS {
            Some(OutlineFailure::PointLimit)
        } else {
            None
        };
        if self.failure.is_some() {
            return;
        }
        self.points += 1;
        self.current.push(point);
    }
    fn finish(mut self) -> Result<Vec<Vec<FontPoint>>, DocsightError> {
        self.close();
        match self.failure {
            Some(OutlineFailure::InvalidCoordinate) => {
                return Err(malformed(
                    "PostScript glyph contains a non-finite coordinate",
                ));
            }
            Some(OutlineFailure::CoordinateLimit) => {
                return Err(DocsightError::ResourceLimit {
                    resource: "PostScript glyph coordinates".to_owned(),
                    limit: 10_000_000,
                });
            }
            Some(OutlineFailure::PointLimit) => {
                return Err(DocsightError::ResourceLimit {
                    resource: "PostScript glyph outline points".to_owned(),
                    limit: MAX_POINTS as u64,
                });
            }
            None => {}
        }
        Ok(self.contours)
    }
}

impl OutlineBuilder for Contours {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.push(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let start = self.point;
        for step in 1..=8 {
            let t = step as f32 / 8.0;
            let u = 1.0 - t;
            self.push(
                u * u * start.x + 2.0 * u * t * x1 + t * t * x,
                u * u * start.y + 2.0 * u * t * y1 + t * t * y,
            );
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let start = self.point;
        for step in 1..=16 {
            let t = step as f32 / 16.0;
            let u = 1.0 - t;
            self.push(
                u * u * u * start.x + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x,
                u * u * u * start.y + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y,
            );
        }
    }
    fn close(&mut self) {
        if !self.current.is_empty() {
            if self.current.first() != self.current.last()
                && let Some(first) = self.current.first().copied()
            {
                if self.points >= MAX_POINTS {
                    self.failure = Some(OutlineFailure::PointLimit);
                } else {
                    self.current.push(first);
                    self.points += 1;
                }
            }
            self.contours.push(std::mem::take(&mut self.current));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PdfDocument;
    use docsight_core::DocumentSource;

    fn type1_program() -> Vec<u8> {
        let charstring = [
            139, 248, 236, 13, 139, 139, 21, 248, 136, 139, 5, 251, 142, 249, 80, 5, 251, 142, 253,
            80, 5, 9, 14,
        ];
        let mut plaintext =
            b"seed /lenIV -1 def /Subrs 0 array /CharStrings 1 dict dup begin /A ".to_vec();
        plaintext.extend_from_slice(format!("{} RD ", charstring.len()).as_bytes());
        plaintext.extend_from_slice(&charstring);
        plaintext.extend_from_slice(b" ND end ");
        let mut state = 55665_u16;
        let encrypted: Vec<u8> = plaintext
            .iter()
            .map(|byte| {
                let cipher = byte ^ (state >> 8) as u8;
                state = u16::from(cipher)
                    .wrapping_add(state)
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                cipher
            })
            .collect();
        let mut data = b"%!PS-AdobeFont-1.0: Sample 1.0\n/FontType 1 def /FontMatrix [0.001 0 0 0.001 0 0] def /Encoding 256 array dup 65 /A put readonly def currentfile eexec\n".to_vec();
        data.extend_from_slice(&encrypted);
        data
    }

    fn cff_program() -> Vec<u8> {
        let charstring = [
            248, 236, 139, 139, 21, 248, 136, 139, 251, 142, 249, 80, 251, 142, 253, 80, 5, 14,
        ];
        let mut data = vec![
            1, 0, 4, 4, 0, 1, 1, 1, 2, b'X', 0, 1, 1, 1, 5, 162, 15, 165, 17, 0, 0, 0, 0, 0, 0, 34,
            0, 2, 1, 1, 2, 20, 14,
        ];
        data.extend_from_slice(&charstring);
        data
    }

    fn sample_pdf(data: &[u8], cff: bool) -> Vec<u8> {
        let content = b"BT /F1 36 Tf 20 60 Td (AA) Tj ET";
        let kind = if cff { "/FontFile3" } else { "/FontFile" };
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Sample /FirstChar 65 /LastChar 65 /Widths [600] /FontDescriptor 5 0 R >>".to_owned();
        let font_entries = if cff { "/Subtype /Type1C" } else { "" };
        let mut font_stream =
            format!("<< /Length {} {font_entries} >>\nstream\n", data.len()).into_bytes();
        font_stream.extend_from_slice(data);
        font_stream.extend_from_slice(b"\nendstream");
        let mut content_stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        content_stream.extend_from_slice(content);
        content_stream.extend_from_slice(b"\nendstream");
        let objects = [b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 7 0 R >>".to_vec(),font.into_bytes(),format!("<< /Type /FontDescriptor /FontName /Sample /Flags 32 {kind} 6 0 R >>").into_bytes(),font_stream,content_stream];
        let mut output = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            output.extend_from_slice(object);
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref = output.len();
        output.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for offset in offsets {
            output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        output.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        output
    }

    #[test]
    fn embedded_type1_and_cff_render_without_external_fonts()
    -> Result<(), Box<dyn std::error::Error>> {
        for (program, cff) in [(type1_program(), false), (cff_program(), true)] {
            let source = DocumentSource::from_bytes(sample_pdf(&program, cff))?;
            let pdf = PdfDocument::open(&source)?;
            let page = pdf.page(1)?;
            assert_eq!(page.spans[0].text, "AA");
            assert!((page.spans[0].bbox.width() - 43.2).abs() < 0.01);
            let document = pdf.to_document()?;
            assert!(
                !document
                    .warnings
                    .iter()
                    .any(|warning| warning.code == "PDF_FONT_OUTLINE_MISSING")
            );
            let first = pdf.rasterize(1, 72, None)?;
            let second = pdf.rasterize(1, 72, None)?;
            assert_eq!(first.png, second.png);
            assert!(first.pixels.iter().any(|value| *value < 128));
        }
        Ok(())
    }

    #[test]
    fn differences_select_the_source_glyph_even_when_unicode_differs() -> Result<(), DocsightError>
    {
        let encoding = Value::Dict(BTreeMap::from([(
            "Differences".to_owned(),
            Value::Array(vec![Value::Int(66), Value::Name("A".to_owned())]),
        )]));
        let program =
            PostScriptProgram::parse(type1_program(), false, Some(&encoding), Some([600.0; 256]))?;
        assert_eq!(program.characters()[66], Some('A'));
        assert_eq!(program.simple(&[65])?, program.simple(&[66])?);
        assert!(program.simple(&[67]).is_err());
        Ok(())
    }

    #[test]
    fn contours_are_closed_and_outline_limits_fail_explicitly() -> Result<(), DocsightError> {
        let mut builder = Contours::new(Matrix::default());
        builder.move_to(0.0, 0.0);
        builder.line_to(500.0, 0.0);
        builder.line_to(250.0, 700.0);
        let contours = builder.finish()?;
        assert_eq!(contours[0].first(), contours[0].last());
        let mut builder = Contours::new(Matrix::default());
        builder.move_to(f32::INFINITY, 0.0);
        assert!(matches!(
            builder.finish(),
            Err(DocsightError::MalformedDocument { .. })
        ));
        assert!(PostScriptProgram::parse(vec![0; MAX_FONT_BYTES + 1], false, None, None).is_err());
        assert!(PostScriptProgram::parse(vec![0; 10], true, None, None).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod outline_boundaries {
    use super::*;

    #[test]
    fn closure_vertices_are_included_in_the_hard_outline_budget() -> Result<(), DocsightError> {
        let mut allowed = Contours::new(Matrix::default());
        allowed.move_to(0.0, 0.0);
        for value in 1..MAX_POINTS - 1 {
            allowed.line_to(value as f32, 1.0);
        }
        assert_eq!(
            allowed.finish()?.iter().map(Vec::len).sum::<usize>(),
            MAX_POINTS
        );
        let mut exceeded = Contours::new(Matrix::default());
        exceeded.move_to(0.0, 0.0);
        for value in 1..MAX_POINTS {
            exceeded.line_to(value as f32, 1.0);
        }
        assert!(
            matches!(exceeded.finish(),Err(DocsightError::ResourceLimit { resource,limit }) if resource=="PostScript glyph outline points" && limit==MAX_POINTS as u64)
        );
        let mut oversized = Contours::new(Matrix::default());
        oversized.move_to(100_000_000.0, 0.0);
        assert!(
            matches!(oversized.finish(),Err(DocsightError::ResourceLimit { resource,limit:10_000_000 }) if resource=="PostScript glyph coordinates")
        );
        Ok(())
    }
}
