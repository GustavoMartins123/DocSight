use docsight_core::DocsightError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub const DEFAULT_FAMILY: &str = "Liberation Sans";
pub const NORMALIZED_SANS: &str = "DejaVu Sans";
pub const NORMALIZED_PROFILE: &str = "docx-normalized-v1";
pub const MAX_FONT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_FONT_FILES: usize = 128;
pub const MAX_SHAPING_CHARACTERS: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub id: u16,
    pub cluster: u32,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapedText {
    pub glyphs: Vec<Glyph>,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
}

#[derive(Clone)]
pub struct FontFace {
    bytes: Arc<[u8]>,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub sha256: String,
    unicode_symbol_encoding: bool,
}

impl std::fmt::Debug for FontFace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FontFace")
            .field("family", &self.family)
            .field("sha256", &self.sha256)
            .finish()
    }
}

impl PartialEq for FontFace {
    fn eq(&self, other: &Self) -> bool {
        self.sha256 == other.sha256
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontBook {
    faces: BTreeMap<(String, bool, bool), FontFace>,
    normalized: bool,
}

pub struct GlyphBitmap {
    pub width: usize,
    pub height: usize,
    pub left: i32,
    pub bottom: i32,
    pub coverage: Vec<u8>,
}

fn font_error(message: impl Into<String>) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "docsight-fonts".to_owned(),
        message: message.into(),
    }
}

impl FontFace {
    pub fn decoded_text(&self, text: &str) -> Result<String, DocsightError> {
        text.chars()
            .map(|character| self.decode_character(character))
            .collect()
    }

    fn decode_character(&self, character: char) -> Result<char, DocsightError> {
        if self.unicode_symbol_encoding && (0xf000..=0xf0ff).contains(&u32::from(character)) {
            decode_symbol_character(character)
        } else {
            Ok(character)
        }
    }

    pub fn underline_metrics(&self, size: f32) -> Result<(f32, f32), DocsightError> {
        let face = rustybuzz::Face::from_slice(&self.bytes, 0)
            .ok_or_else(|| font_error("validated font could not be reopened"))?;
        let metrics =
            face.underline_metrics()
                .ok_or_else(|| DocsightError::UnsupportedFeature {
                    feature: format!("font {} has no underline metrics", self.family),
                })?;
        let scale = size / face.units_per_em() as f32;
        Ok((
            -metrics.position as f32 * scale,
            metrics.thickness as f32 * scale,
        ))
    }
    pub fn has_glyph(&self, character: char) -> Result<bool, DocsightError> {
        let face = rustybuzz::Face::from_slice(&self.bytes, 0)
            .ok_or_else(|| font_error("validated font could not be reopened"))?;
        Ok(face
            .glyph_index(self.decode_character(character)?)
            .is_some())
    }
    pub fn glyph_coverage(&self, text: &str) -> Result<(u64, u64), DocsightError> {
        let face = rustybuzz::Face::from_slice(&self.bytes, 0)
            .ok_or_else(|| font_error("validated font could not be reopened"))?;
        let mut total = 0_u64;
        let mut covered = 0_u64;
        for character in text.chars().filter(|character| !character.is_control()) {
            total += 1;
            covered += u64::from(
                face.glyph_index(self.decode_character(character)?)
                    .is_some(),
            );
        }
        Ok((covered, total))
    }

    fn parse(bytes: Arc<[u8]>) -> Result<Self, DocsightError> {
        if bytes.len() as u64 > MAX_FONT_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "font bytes".to_owned(),
                limit: MAX_FONT_BYTES,
            });
        }
        let face = rustybuzz::Face::from_slice(&bytes, 0)
            .ok_or_else(|| font_error("font is not a valid OpenType face"))?;
        let family = face
            .names()
            .into_iter()
            .filter(|name| name.name_id == 16 || name.name_id == 1)
            .filter_map(|name| name.to_string())
            .next()
            .ok_or_else(|| font_error("font has no Unicode family name"))?;
        let unicode_symbol_encoding = family.eq_ignore_ascii_case("Symbol")
            && !face.tables().cmap.is_some_and(|table| {
                table.subtables.into_iter().any(|subtable| {
                    subtable.platform_id == rustybuzz::ttf_parser::PlatformId::Windows
                        && subtable.encoding_id == 0
                })
            });
        let bold = face.is_bold();
        let italic = face.is_italic();
        let sha256 = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            bytes,
            family,
            bold,
            italic,
            sha256,
            unicode_symbol_encoding,
        })
    }

    pub fn shape(&self, text: &str, size: f32) -> Result<ShapedText, DocsightError> {
        if !size.is_finite() || size <= 0.0 || size > 4096.0 {
            return Err(font_error("font size must be finite and within 0..4096 pt"));
        }
        if text.chars().count() > MAX_SHAPING_CHARACTERS {
            return Err(DocsightError::ResourceLimit {
                resource: "shaping characters".to_owned(),
                limit: MAX_SHAPING_CHARACTERS as u64,
            });
        }
        let face = rustybuzz::Face::from_slice(&self.bytes, 0)
            .ok_or_else(|| font_error("validated font could not be reopened"))?;
        let scale = size / face.units_per_em() as f32;
        let mut decoded = String::new();
        let mut source_offsets = Vec::new();
        for (offset, character) in text.char_indices() {
            let character = self.decode_character(character)?;
            source_offsets.extend(std::iter::repeat_n(offset, character.len_utf8()));
            decoded.push(character);
        }
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(&decoded);
        buffer.guess_segment_properties();
        let shaped = rustybuzz::shape(&face, &[], buffer);
        let mut x = 0.0;
        let mut glyphs = Vec::with_capacity(shaped.len());
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let id = u16::try_from(info.glyph_id)
                .map_err(|_| font_error("shaper returned a glyph outside OpenType bounds"))?;
            let source_offset = *source_offsets
                .get(info.cluster as usize)
                .ok_or_else(|| font_error("shaper cluster exceeds source text"))?;
            if id == 0
                && text
                    .get(source_offset..)
                    .and_then(|value| value.chars().next())
                    .is_some_and(|character| !character.is_control())
            {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!(
                        "font {} has no glyph for {:?} at UTF-8 offset {}",
                        self.family,
                        text.get(source_offset..)
                            .and_then(|value| value.chars().next()),
                        source_offset
                    ),
                });
            }
            let advance = position.x_advance as f32 * scale;
            glyphs.push(Glyph {
                id,
                cluster: u32::try_from(source_offset)
                    .map_err(|_| font_error("font cluster exceeds source bounds"))?,
                x: x + position.x_offset as f32 * scale,
                y: position.y_offset as f32 * scale,
                advance,
            });
            x += advance;
        }
        Ok(ShapedText {
            glyphs,
            width: x,
            ascent: face.ascender() as f32 * scale,
            descent: -face.descender() as f32 * scale,
            line_gap: face.line_gap() as f32 * scale,
        })
    }

    pub fn rasterize(&self, id: u16, pixels_per_em: f32) -> Result<GlyphBitmap, DocsightError> {
        self.rasterizer()?.rasterize(id, pixels_per_em)
    }

    pub fn rasterizer(&self) -> Result<RasterFont, DocsightError> {
        Ok(RasterFont(
            fontdue::Font::from_bytes(self.bytes.clone(), fontdue::FontSettings::default())
                .map_err(font_error)?,
        ))
    }
}

pub struct RasterFont(fontdue::Font);

impl RasterFont {
    pub fn rasterize(&self, id: u16, pixels_per_em: f32) -> Result<GlyphBitmap, DocsightError> {
        if !pixels_per_em.is_finite() || !(0.0..=4096.0).contains(&pixels_per_em) {
            return Err(font_error("glyph raster size is outside 0..4096 pixels"));
        }
        let metrics = self.0.metrics_indexed(id, pixels_per_em);
        let pixels = metrics
            .width
            .checked_mul(metrics.height)
            .filter(|count| *count <= 16_777_216)
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "glyph raster pixels".to_owned(),
                limit: 16_777_216,
            })?;
        let (metrics, coverage) = self.0.rasterize_indexed(id, pixels_per_em);
        if coverage.len() != pixels {
            return Err(font_error(
                "glyph raster buffer disagrees with its dimensions",
            ));
        }
        Ok(GlyphBitmap {
            width: metrics.width,
            height: metrics.height,
            left: metrics.xmin,
            bottom: metrics.ymin,
            coverage,
        })
    }
}

impl FontBook {
    pub fn bundled() -> Result<Self, DocsightError> {
        let mut book = Self {
            faces: BTreeMap::new(),
            normalized: false,
        };
        for bytes in BUNDLED {
            book.insert(Arc::from(*bytes))?;
        }
        Ok(book)
    }

    pub fn normalized() -> Result<Self, DocsightError> {
        let mut book = Self::bundled()?;
        book.normalized = true;
        Ok(book)
    }

    pub fn read_directory(&mut self, directory: &Path) -> Result<(), DocsightError> {
        let entries = std::fs::read_dir(directory).map_err(|source| DocsightError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|source| DocsightError::Io {
                    path: directory.to_path_buf(),
                    source,
                })?
                .path();
            if path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("ttf") || extension.eq_ignore_ascii_case("otf")
            }) {
                paths.push(path);
                if paths.len() > MAX_FONT_FILES {
                    return Err(DocsightError::ResourceLimit {
                        resource: "font files".to_owned(),
                        limit: MAX_FONT_FILES as u64,
                    });
                }
            }
        }
        paths.sort();
        for path in paths {
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|source| DocsightError::Io {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.is_file() {
                return Err(font_error(
                    "font path must be a regular file without symlinks",
                ));
            }
            if metadata.len() > MAX_FONT_BYTES {
                return Err(DocsightError::ResourceLimit {
                    resource: "font bytes".to_owned(),
                    limit: MAX_FONT_BYTES,
                });
            }
            let file = std::fs::File::open(&path).map_err(|source| DocsightError::Io {
                path: path.clone(),
                source,
            })?;
            let mut bytes = Vec::new();
            file.take(MAX_FONT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| DocsightError::Io { path, source })?;
            self.insert(Arc::from(bytes))?;
        }
        Ok(())
    }

    fn insert(&mut self, bytes: Arc<[u8]>) -> Result<(), DocsightError> {
        let face = FontFace::parse(bytes)?;
        let key = (face.family.to_lowercase(), face.bold, face.italic);
        if let Some(previous) = self.faces.get(&key) {
            if previous.sha256 != face.sha256 {
                return Err(font_error(format!(
                    "font {} has ambiguous files for the same weight and slant",
                    face.family
                )));
            }
        } else {
            self.faces.insert(key, face);
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        family: &str,
        bold: bool,
        italic: bool,
    ) -> Result<FontFace, DocsightError> {
        let selected = if self.normalized {
            normalized_family(family)
        } else {
            family
        };
        let mut face = self
            .faces
            .get(&(selected.to_lowercase(), bold, italic))
            .cloned()
            .ok_or_else(|| DocsightError::FontUnavailable {
                font: docsight_core::FontRequirement {
                    family: family.to_owned(),
                    bold,
                    italic,
                },
                location: None,
            })?;
        if self.normalized && family.eq_ignore_ascii_case("Symbol") {
            face.unicode_symbol_encoding = true;
        }
        Ok(face)
    }

    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"docsight-fonts/rustybuzz-0.20.1/fontdue-0.9.3/v1");
        if self.normalized {
            digest.update(NORMALIZED_PROFILE.as_bytes());
        }
        for (key, face) in &self.faces {
            digest.update(key.0.as_bytes());
            digest.update([u8::from(key.1), u8::from(key.2)]);
            digest.update(face.sha256.as_bytes());
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

pub fn normalized_family(family: &str) -> &'static str {
    match family.to_ascii_lowercase().as_str() {
        "liberation mono" | "courier" | "courier new" | "consolas" | "cascadia code"
        | "cascadia mono" | "menlo" | "monaco" | "dejavu sans mono" | "noto sans mono" => {
            "Liberation Mono"
        }
        "liberation serif" | "times" | "times new roman" | "georgia" | "cambria" | "palatino"
        | "palatino linotype" | "book antiqua" | "dejavu serif" | "noto serif" => {
            "Liberation Serif"
        }
        _ => NORMALIZED_SANS,
    }
}

const BUNDLED: &[&[u8]] = &[
    include_bytes!("../assets/DejaVuSans.ttf"),
    include_bytes!("../assets/DejaVuSans-Bold.ttf"),
    include_bytes!("../assets/DejaVuSans-Oblique.ttf"),
    include_bytes!("../assets/DejaVuSans-BoldOblique.ttf"),
    include_bytes!("../assets/LiberationSans-Regular.ttf"),
    include_bytes!("../assets/LiberationSans-Bold.ttf"),
    include_bytes!("../assets/LiberationSans-Italic.ttf"),
    include_bytes!("../assets/LiberationSans-BoldItalic.ttf"),
    include_bytes!("../assets/LiberationSerif-Regular.ttf"),
    include_bytes!("../assets/LiberationSerif-Bold.ttf"),
    include_bytes!("../assets/LiberationSerif-Italic.ttf"),
    include_bytes!("../assets/LiberationSerif-BoldItalic.ttf"),
    include_bytes!("../assets/LiberationMono-Regular.ttf"),
    include_bytes!("../assets/LiberationMono-Bold.ttf"),
    include_bytes!("../assets/LiberationMono-Italic.ttf"),
    include_bytes!("../assets/LiberationMono-BoldItalic.ttf"),
];

const SYMBOL_ENCODING: [u32; 256] = [
    0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x0008, 0x0009, 0x000a, 0x000b,
    0x000c, 0x000d, 0x000e, 0x000f, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016, 0x0017,
    0x0018, 0x0019, 0x001a, 0x001b, 0x001c, 0x001d, 0x001e, 0x001f, 0x0020, 0x0021, 0x2200, 0x0023,
    0x2203, 0x0025, 0x0026, 0x220d, 0x0028, 0x0029, 0x2217, 0x002b, 0x002c, 0x2212, 0x002e, 0x002f,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x003a, 0x003b,
    0x003c, 0x003d, 0x003e, 0x003f, 0x2245, 0x0391, 0x0392, 0x03a7, 0x0394, 0x0395, 0x03a6, 0x0393,
    0x0397, 0x0399, 0x03d1, 0x039a, 0x039b, 0x039c, 0x039d, 0x039f, 0x03a0, 0x0398, 0x03a1, 0x03a3,
    0x03a4, 0x03a5, 0x03c2, 0x03a9, 0x039e, 0x03a8, 0x0396, 0x005b, 0x2234, 0x005d, 0x22a5, 0x005f,
    0xf8e5, 0x03b1, 0x03b2, 0x03c7, 0x03b4, 0x03b5, 0x03c6, 0x03b3, 0x03b7, 0x03b9, 0x03d5, 0x03ba,
    0x03bb, 0x03bc, 0x03bd, 0x03bf, 0x03c0, 0x03b8, 0x03c1, 0x03c3, 0x03c4, 0x03c5, 0x03d6, 0x03c9,
    0x03be, 0x03c8, 0x03b6, 0x007b, 0x007c, 0x007d, 0x223c, 0x007f, 0x0080, 0x0081, 0x0082, 0x0083,
    0x0084, 0x0085, 0x0086, 0x0087, 0x0088, 0x0089, 0x008a, 0x008b, 0x008c, 0x008d, 0x008e, 0x008f,
    0x0090, 0x0091, 0x0092, 0x0093, 0x0094, 0x0095, 0x0096, 0x0097, 0x0098, 0x0099, 0x009a, 0x009b,
    0x009c, 0x009d, 0x009e, 0x009f, 0x0000, 0x03d2, 0x2032, 0x2264, 0x2044, 0x221e, 0x0192, 0x2663,
    0x2666, 0x2665, 0x2660, 0x2194, 0x2190, 0x2191, 0x2192, 0x2193, 0x00b0, 0x00b1, 0x2033, 0x2265,
    0x00d7, 0x221d, 0x2202, 0x2022, 0x00f7, 0x2260, 0x2261, 0x2248, 0x2026, 0xf8e6, 0xf8e7, 0x21b5,
    0x2135, 0x2111, 0x211c, 0x2118, 0x2297, 0x2295, 0x2205, 0x2229, 0x222a, 0x2283, 0x2287, 0x2284,
    0x2282, 0x2286, 0x2208, 0x2209, 0x2220, 0x2207, 0x00ae, 0x00a9, 0x2122, 0x220f, 0x221a, 0x22c5,
    0x00ac, 0x2227, 0x2228, 0x21d4, 0x21d0, 0x21d1, 0x21d2, 0x21d3, 0x22c4, 0x2329, 0xf8e8, 0xf8e9,
    0xf8ea, 0x2211, 0xf8eb, 0xf8ec, 0xf8ed, 0xf8ee, 0xf8ef, 0xf8f0, 0xf8f1, 0xf8f2, 0xf8f3, 0xf8f4,
    0xf8ff, 0x232a, 0x222b, 0x2320, 0xf8f5, 0x2321, 0xf8f6, 0xf8f7, 0xf8f8, 0xf8f9, 0xf8fa, 0xf8fb,
    0xf8fc, 0xf8fd, 0xf8fe, 0x0000,
];

fn decode_symbol_character(character: char) -> Result<char, DocsightError> {
    let index = u32::from(character)
        .checked_sub(0xf000)
        .filter(|index| *index < 256)
        .ok_or_else(|| font_error("Symbol character is outside its private encoding"))?;
    let value = SYMBOL_ENCODING[index as usize];
    char::from_u32(value)
        .filter(|value| !value.is_control())
        .ok_or_else(|| DocsightError::UnsupportedFeature {
            feature: format!(
                "unassigned Symbol encoding character U+{:04X}",
                u32::from(character)
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_size_and_file_count_limits_are_enforced_at_the_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("face.ttf");
        let mut bytes = BUNDLED[0].to_vec();
        bytes.resize(usize::try_from(MAX_FONT_BYTES)?, 0);
        std::fs::write(&path, &bytes)?;
        FontBook {
            faces: BTreeMap::new(),
            normalized: false,
        }
        .read_directory(directory.path())?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(MAX_FONT_BYTES + 1)?;
        assert!(
            matches!(FontBook::bundled()?.read_directory(directory.path()), Err(DocsightError::ResourceLimit { resource, limit: MAX_FONT_BYTES }) if resource == "font bytes")
        );
        std::fs::remove_file(path)?;
        for index in 0..MAX_FONT_FILES {
            std::fs::write(directory.path().join(format!("{index:03}.ttf")), BUNDLED[0])?;
        }
        FontBook::bundled()?.read_directory(directory.path())?;
        std::fs::write(directory.path().join("extra.ttf"), BUNDLED[0])?;
        assert!(
            matches!(FontBook::bundled()?.read_directory(directory.path()), Err(DocsightError::ResourceLimit { resource, limit }) if resource == "font files" && limit == MAX_FONT_FILES as u64)
        );
        Ok(())
    }

    #[test]
    fn invalid_faces_sizes_and_changed_duplicate_faces_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("face.ttf");
        std::fs::write(&path, b"invalid OpenType")?;
        assert!(
            FontBook::bundled()?
                .read_directory(directory.path())
                .is_err()
        );
        let mut changed = BUNDLED[0].to_vec();
        changed.push(0);
        std::fs::write(&path, &changed)?;
        assert!(
            FontBook::bundled()?
                .read_directory(directory.path())
                .is_err()
        );
        let face = FontBook::bundled()?.resolve(DEFAULT_FAMILY, false, false)?;
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY, 4097.0] {
            assert!(face.shape("text", size).is_err());
        }
        assert!(face.shape("text", 4096.0).is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn font_file_symlinks_are_not_followed() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("font.bin");
        std::fs::write(&target, BUNDLED[0])?;
        std::os::unix::fs::symlink(&target, directory.path().join("linked.ttf"))?;
        assert!(
            FontBook::bundled()?
                .read_directory(directory.path())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn symbol_private_encoding_is_decoded_without_changing_font_family() -> Result<(), DocsightError>
    {
        assert_eq!(decode_symbol_character('\u{f0b7}')?, '•');
        assert_eq!(decode_symbol_character('\u{f041}')?, 'Α');
        assert!(decode_symbol_character('\u{f000}').is_err());
        Ok(())
    }

    #[test]
    fn real_metrics_shaping_and_raster_share_one_face() -> Result<(), DocsightError> {
        let book = FontBook::bundled()?;
        let face = book.resolve(DEFAULT_FAMILY, false, false)?;
        let shaped = face.shape("office Café", 12.0)?;
        assert!(shaped.width > 30.0);
        assert!(face.shape("WW", 12.0)?.width > face.shape("ii", 12.0)?.width);
        let raster = face.rasterize(shaped.glyphs[0].id, 24.0)?;
        assert!(
            raster
                .coverage
                .iter()
                .any(|alpha| *alpha > 0 && *alpha < 255)
        );
        assert_eq!(book.fingerprint(), FontBook::bundled()?.fingerprint());
        Ok(())
    }

    #[test]
    fn missing_faces_and_glyphs_fail_without_substitution() -> Result<(), DocsightError> {
        let book = FontBook::bundled()?;
        assert!(book.resolve("Absent Font", false, false).is_err());
        assert!(
            book.resolve(DEFAULT_FAMILY, false, false)?
                .shape("\u{10FFFF}", 12.0)
                .is_err()
        );
        Ok(())
    }
}

#[cfg(test)]
mod normalization_tests {
    use super::*;

    #[test]
    fn normalization_is_an_upfront_policy_not_missing_font_recovery() -> Result<(), DocsightError> {
        let normalized = FontBook::normalized()?;
        let exact = FontBook::bundled()?;
        for name in [
            "Arial",
            "Aptos",
            "Calibri",
            "Uninstalled Family",
            "Liberation Sans",
        ] {
            let face = normalized.resolve(name, true, true)?;
            assert_eq!(face.family, NORMALIZED_SANS);
            assert_eq!(
                face.sha256,
                exact.resolve(NORMALIZED_SANS, true, true)?.sha256
            );
        }
        assert_eq!(
            normalized.resolve("Courier New", false, false)?.family,
            "Liberation Mono"
        );
        assert_eq!(
            normalized.resolve("Times New Roman", false, false)?.family,
            "Liberation Serif"
        );
        assert_ne!(normalized.fingerprint(), exact.fingerprint());
        assert!(matches!(
            exact.resolve("Uninstalled Family", false, false),
            Err(DocsightError::FontUnavailable { .. })
        ));
        let error = exact
            .resolve("Uninstalled Family", false, false)
            .err()
            .ok_or_else(|| font_error("exact font resolution unexpectedly succeeded"))?;
        assert!(error.to_string().contains("configured font book"));
        assert!(!error.to_string().contains("DOCSIGHT_FONT_DIR"));
        Ok(())
    }

    #[test]
    fn normalized_symbol_encoding_is_decoded_before_drawing() -> Result<(), DocsightError> {
        let face = FontBook::normalized()?.resolve("Symbol", false, false)?;
        assert_eq!(face.decoded_text("\u{f0b7}")?, "•");
        assert!(face.shape("\u{f0b7}", 11.0)?.width > 0.0);
        assert!(face.shape("\u{10ffff}", 11.0).is_err());
        Ok(())
    }
}
