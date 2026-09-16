use docsight_core::DocsightError;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use zip::ZipArchive;

pub const MAX_PACKAGE_ENTRIES: usize = 2_048;
pub const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_XML_PART_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BINARY_PART_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_COMPRESSION_RATIO: u64 = 200;
const CENTRAL_DIRECTORY_HEADER_BYTES: usize = 46;
const END_OF_CENTRAL_DIRECTORY_BYTES: usize = 22;

pub struct DocxParts {
    pub document: String,
    pub styles: Option<String>,
    pub numbering: Option<String>,
    pub rels: Option<String>,
    pub headers: Vec<(String, String)>,
    pub footers: Vec<(String, String)>,
    pub footnotes: Option<String>,
    pub endnotes: Option<String>,
    pub comments: Option<String>,
    pub core_properties: Option<String>,
    pub extended_properties: Option<String>,
    pub binary_part_digests: std::collections::BTreeMap<String, String>,
    pub binary_part_formats: std::collections::BTreeMap<String, String>,
    pub inert_part_digests: std::collections::BTreeMap<String, String>,
}

pub fn read_parts(bytes: &[u8]) -> Result<DocxParts, DocsightError> {
    preflight_archive(bytes)?;
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(zip_error)?;
    validate_archive(&mut archive)?;
    require_part(&mut archive, "[Content_Types].xml")?;
    let document = read_xml_part(&mut archive, "word/document.xml")?.ok_or_else(|| {
        DocsightError::MalformedDocument {
            message: "required OOXML part is missing: word/document.xml".to_owned(),
        }
    })?;
    let styles = read_xml_part(&mut archive, "word/styles.xml")?;
    let numbering = read_xml_part(&mut archive, "word/numbering.xml")?;
    let rels = read_xml_part(&mut archive, "word/_rels/document.xml.rels")?;
    let footnotes = read_xml_part(&mut archive, "word/footnotes.xml")?;
    let endnotes = read_xml_part(&mut archive, "word/endnotes.xml")?;
    let comments = read_xml_part(&mut archive, "word/comments.xml")?;
    let core_properties = read_xml_part(&mut archive, "docProps/core.xml")?;
    let extended_properties = read_xml_part(&mut archive, "docProps/app.xml")?;

    let mut header_names = Vec::new();
    let mut footer_names = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(zip_error)?;
        let name = file.name();
        if name.starts_with("word/header") && name.ends_with(".xml") {
            header_names.push(name.to_owned());
        } else if name.starts_with("word/footer") && name.ends_with(".xml") {
            footer_names.push(name.to_owned());
        }
    }

    let mut headers = Vec::new();
    for name in header_names {
        if let Some(text) = read_xml_part(&mut archive, &name)? {
            headers.push((name, text));
        }
    }

    let mut footers = Vec::new();
    for name in footer_names {
        if let Some(text) = read_xml_part(&mut archive, &name)? {
            footers.push((name, text));
        }
    }

    let mut binary_names = Vec::new();
    let mut inert_names = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(zip_error)?;
        if !file.is_dir() {
            if file.name().starts_with("word/media/") || file.name().starts_with("media/") {
                binary_names.push(file.name().to_owned());
            }
            if is_inert_part(file.name()) {
                inert_names.push(file.name().to_owned());
            }
        }
    }
    binary_names.sort();
    let mut binary_part_digests = std::collections::BTreeMap::new();
    let mut binary_part_formats = std::collections::BTreeMap::new();
    for name in binary_names {
        let (digest, format) = hash_binary_part(&mut archive, &name)?;
        binary_part_formats.insert(name.clone(), format);
        binary_part_digests.insert(name, digest);
    }
    inert_names.sort();
    let mut inert_part_digests = std::collections::BTreeMap::new();
    for name in inert_names {
        let (digest, _) = hash_binary_part(&mut archive, &name)?;
        inert_part_digests.insert(name, digest);
    }

    Ok(DocxParts {
        document,
        styles,
        numbering,
        rels,
        headers,
        footers,
        footnotes,
        endnotes,
        comments,
        core_properties,
        extended_properties,
        binary_part_digests,
        binary_part_formats,
        inert_part_digests,
    })
}

fn preflight_archive(bytes: &[u8]) -> Result<(), DocsightError> {
    let end_record = find_end_of_central_directory(bytes)?;
    let disk = read_u16(bytes, end_record + 4)?;
    let central_disk = read_u16(bytes, end_record + 6)?;
    let disk_entries = read_u16(bytes, end_record + 8)?;
    let total_entries = read_u16(bytes, end_record + 10)?;
    if disk != 0 || central_disk != 0 || disk_entries != total_entries {
        return Err(archive_metadata_error(
            "multi-disk OPC packages are unsupported",
        ));
    }
    if usize::from(total_entries) > MAX_PACKAGE_ENTRIES {
        return Err(resource_limit(
            "package entry count",
            MAX_PACKAGE_ENTRIES as u64,
        ));
    }
    let central_size = usize::try_from(read_u32(bytes, end_record + 12)?)
        .map_err(|_| archive_metadata_error("central directory size overflow"))?;
    let central_offset = usize::try_from(read_u32(bytes, end_record + 16)?)
        .map_err(|_| archive_metadata_error("central directory offset overflow"))?;
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or_else(|| archive_metadata_error("central directory range overflow"))?;
    if central_end != end_record {
        return Err(archive_metadata_error(
            "central directory range does not end at its terminal record",
        ));
    }

    let mut cursor = central_offset;
    let mut total_uncompressed = 0_u64;
    for _ in 0..total_entries {
        let fixed_end = cursor
            .checked_add(CENTRAL_DIRECTORY_HEADER_BYTES)
            .ok_or_else(|| archive_metadata_error("central directory entry range overflow"))?;
        let fixed = bytes
            .get(cursor..fixed_end)
            .ok_or_else(|| archive_metadata_error("truncated central directory entry"))?;
        if fixed.get(..4) != Some(b"PK\x01\x02") {
            return Err(archive_metadata_error(
                "invalid central directory entry signature",
            ));
        }
        let compressed = u64::from(read_u32(bytes, cursor + 20)?);
        let uncompressed = u64::from(read_u32(bytes, cursor + 24)?);
        let name_length = usize::from(read_u16(bytes, cursor + 28)?);
        let extra_length = usize::from(read_u16(bytes, cursor + 30)?);
        let comment_length = usize::from(read_u16(bytes, cursor + 32)?);
        let start_disk = read_u16(bytes, cursor + 34)?;
        let local_offset = read_u32(bytes, cursor + 42)?;
        if compressed == u64::from(u32::MAX)
            || uncompressed == u64::from(u32::MAX)
            || local_offset == u32::MAX
        {
            return Err(archive_metadata_error(
                "ZIP64 OPC package metadata is unsupported",
            ));
        }
        if start_disk != 0 {
            return Err(archive_metadata_error(
                "multi-disk OPC package entry is unsupported",
            ));
        }
        if uncompressed > 0
            && (compressed == 0 || uncompressed > compressed.saturating_mul(MAX_COMPRESSION_RATIO))
        {
            return Err(resource_limit(
                "package compression ratio",
                MAX_COMPRESSION_RATIO,
            ));
        }
        total_uncompressed = total_uncompressed
            .checked_add(uncompressed)
            .ok_or_else(|| {
                resource_limit("package uncompressed size", MAX_TOTAL_UNCOMPRESSED_BYTES)
            })?;
        if total_uncompressed > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(resource_limit(
                "package uncompressed size",
                MAX_TOTAL_UNCOMPRESSED_BYTES,
            ));
        }
        cursor = fixed_end
            .checked_add(name_length)
            .and_then(|value| value.checked_add(extra_length))
            .and_then(|value| value.checked_add(comment_length))
            .ok_or_else(|| archive_metadata_error("central directory entry range overflow"))?;
        if cursor > central_end {
            return Err(archive_metadata_error(
                "central directory entry exceeds its declared range",
            ));
        }
    }
    if cursor != central_end {
        return Err(archive_metadata_error(
            "central directory entry count does not match its declared range",
        ));
    }
    Ok(())
}

fn find_end_of_central_directory(bytes: &[u8]) -> Result<usize, DocsightError> {
    if bytes.len() < END_OF_CENTRAL_DIRECTORY_BYTES {
        return Err(archive_metadata_error(
            "end of central directory record is missing",
        ));
    }
    let last_start = bytes.len() - END_OF_CENTRAL_DIRECTORY_BYTES;
    let first_start = bytes
        .len()
        .saturating_sub(END_OF_CENTRAL_DIRECTORY_BYTES + usize::from(u16::MAX));
    for offset in (first_start..=last_start).rev() {
        if bytes.get(offset..offset + 4) != Some(b"PK\x05\x06") {
            continue;
        }
        let comment_length = usize::from(read_u16(bytes, offset + 20)?);
        let record_end = offset
            .checked_add(END_OF_CENTRAL_DIRECTORY_BYTES)
            .and_then(|value| value.checked_add(comment_length))
            .ok_or_else(|| archive_metadata_error("end record range overflow"))?;
        if record_end == bytes.len() {
            return Ok(offset);
        }
    }
    Err(archive_metadata_error(
        "valid end of central directory record is missing",
    ))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, DocsightError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| archive_metadata_error("ZIP metadata offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| archive_metadata_error("truncated ZIP metadata"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DocsightError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| archive_metadata_error("ZIP metadata offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| archive_metadata_error("truncated ZIP metadata"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn archive_metadata_error(message: &str) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("invalid OPC package: {message}"),
    }
}

fn is_inert_part(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "word/vbaproject.bin"
        || lower == "word/vbadata.xml"
        || lower.starts_with("word/embeddings/")
        || lower.starts_with("word/activex/")
}

pub fn read_media_part(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>, DocsightError> {
    preflight_archive(bytes)?;
    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).map_err(zip_error)?;
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(zip_error(error)),
    };
    if file.size() > MAX_BINARY_PART_BYTES {
        return Err(resource_limit(
            "binary OOXML part size",
            MAX_BINARY_PART_BYTES,
        ));
    }
    let mut content = Vec::new();
    file.by_ref()
        .take(MAX_BINARY_PART_BYTES.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("failed to read OOXML binary part {name}: {error}"),
        })?;
    if content.len() as u64 > MAX_BINARY_PART_BYTES {
        return Err(resource_limit(
            "binary OOXML part size",
            MAX_BINARY_PART_BYTES,
        ));
    }
    Ok(Some(content))
}

fn detect_image_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "gif"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        "tiff"
    } else if bytes.starts_with(&[0x01, 0x00, 0x00, 0x00]) {
        "emf"
    } else if bytes.starts_with(&[0xD7, 0xCD, 0xC6, 0x9A]) {
        "wmf"
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        "svg"
    } else {
        "unknown"
    }
}

fn hash_binary_part(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<(String, String), DocsightError> {
    let mut file = archive.by_name(name).map_err(zip_error)?;
    if file.size() > MAX_BINARY_PART_BYTES {
        return Err(resource_limit(
            "binary OOXML part size",
            MAX_BINARY_PART_BYTES,
        ));
    }
    let mut reader = file.by_ref().take(MAX_BINARY_PART_BYTES.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    let mut format = "unknown";
    let mut first_chunk = true;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| DocsightError::MalformedDocument {
                message: format!("failed to read OOXML binary part {name}: {error}"),
            })?;
        if read == 0 {
            break;
        }
        if first_chunk {
            format = detect_image_format(&buffer[..read]);
            first_chunk = false;
        }
        total = total
            .checked_add(
                u64::try_from(read)
                    .map_err(|_| resource_limit("binary OOXML part size", MAX_BINARY_PART_BYTES))?,
            )
            .ok_or_else(|| resource_limit("binary OOXML part size", MAX_BINARY_PART_BYTES))?;
        if total > MAX_BINARY_PART_BYTES {
            return Err(resource_limit(
                "binary OOXML part size",
                MAX_BINARY_PART_BYTES,
            ));
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((digest, format.to_owned()))
}

fn validate_archive(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<(), DocsightError> {
    if archive.len() > MAX_PACKAGE_ENTRIES {
        return Err(resource_limit(
            "package entry count",
            MAX_PACKAGE_ENTRIES as u64,
        ));
    }
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(zip_error)?;
        validate_part_name(file.name(), file.is_dir())?;
        if !names.insert(file.name().to_owned()) {
            return Err(DocsightError::MalformedDocument {
                message: format!("duplicate package part: {}", file.name()),
            });
        }
        let size = file.size();
        let compressed = file.compressed_size();
        if size > 0 && (compressed == 0 || size > compressed.saturating_mul(MAX_COMPRESSION_RATIO))
        {
            return Err(resource_limit(
                "package compression ratio",
                MAX_COMPRESSION_RATIO,
            ));
        }
        total = total.checked_add(size).ok_or_else(|| {
            resource_limit("package uncompressed size", MAX_TOTAL_UNCOMPRESSED_BYTES)
        })?;
        if total > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(resource_limit(
                "package uncompressed size",
                MAX_TOTAL_UNCOMPRESSED_BYTES,
            ));
        }
    }
    Ok(())
}

fn validate_part_name(name: &str, directory: bool) -> Result<(), DocsightError> {
    let normalized = if directory {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    let invalid = normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains('\\')
        || normalized.contains(':')
        || normalized.split('/').any(path_segment_invalid);
    if invalid {
        return Err(DocsightError::MalformedDocument {
            message: format!("invalid package part path: {name}"),
        });
    }
    Ok(())
}

fn path_segment_invalid(segment: &str) -> bool {
    let normalized = segment.to_ascii_lowercase();
    segment.is_empty()
        || segment == "."
        || segment == ".."
        || normalized == "%2e"
        || normalized == "%2e%2e"
        || normalized == ".%2e"
        || normalized == "%2e."
        || normalized.contains("%2f")
        || normalized.contains("%5c")
}

fn require_part(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<(), DocsightError> {
    if archive.by_name(name).is_err() {
        return Err(DocsightError::MalformedDocument {
            message: format!("required OOXML part is missing: {name}"),
        });
    }
    Ok(())
}

fn read_xml_part(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, DocsightError> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(zip_error(error)),
    };
    if file.size() > MAX_XML_PART_BYTES {
        return Err(resource_limit("XML part size", MAX_XML_PART_BYTES));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_XML_PART_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("failed to read OOXML part {name}: {error}"),
        })?;
    if bytes.len() as u64 > MAX_XML_PART_BYTES {
        return Err(resource_limit("XML part size", MAX_XML_PART_BYTES));
    }
    let text = String::from_utf8(bytes).map_err(|error| DocsightError::MalformedDocument {
        message: format!("OOXML part is not UTF-8: {name}: {error}"),
    })?;
    if text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        return Err(DocsightError::MalformedDocument {
            message: format!("DTD or entity declarations are forbidden in OOXML part: {name}"),
        });
    }
    Ok(Some(text))
}

fn resource_limit(resource: &str, limit: u64) -> DocsightError {
    DocsightError::ResourceLimit {
        resource: resource.to_owned(),
        limit,
    }
}

fn zip_error(error: zip::result::ZipError) -> DocsightError {
    DocsightError::MalformedDocument {
        message: format!("invalid OPC package: {error}"),
    }
}
