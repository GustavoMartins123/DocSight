use docsight_core::DocsightError;
use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MAX_PACKAGE_ENTRIES: usize = 2_048;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_XML_PART_BYTES: u64 = 16 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;

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
}

pub fn read_parts(bytes: &[u8]) -> Result<DocxParts, DocsightError> {
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

    let mut header_names = Vec::new();
    let mut footer_names = Vec::new();
    for index in 0..archive.len() {
        if let Ok(file) = archive.by_index(index) {
            let name = file.name();
            if name.starts_with("word/header") && name.ends_with(".xml") {
                header_names.push(name.to_owned());
            } else if name.starts_with("word/footer") && name.ends_with(".xml") {
                footer_names.push(name.to_owned());
            }
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
    })
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
