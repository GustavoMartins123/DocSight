#![allow(dead_code)]

use docsight_core::{Document, DocumentSource};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub const MAX_FUZZ_INPUT_BYTES: usize = 1024 * 1024;

pub fn bounded(data: &[u8]) -> &[u8] {
    &data[..data.len().min(MAX_FUZZ_INPUT_BYTES)]
}

pub fn mutate_base(base: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mutated = base.to_vec();
    if mutated.is_empty() {
        return mutated;
    }
    for chunk in bounded(data).chunks(3).take(16_384) {
        let [high, low, value] = chunk else {
            continue;
        };
        let index = ((usize::from(*high) << 8) | usize::from(*low)) % mutated.len();
        mutated[index] ^= *value;
    }
    mutated
}

pub fn mutate_tail(base: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mutated = base.to_vec();
    if mutated.is_empty() {
        return mutated;
    }
    let start = mutated.len() / 2;
    let span = mutated.len() - start;
    for chunk in bounded(data).chunks(3).take(16_384) {
        let [high, low, value] = chunk else {
            continue;
        };
        let index = start + ((usize::from(*high) << 8) | usize::from(*low)) % span;
        mutated[index] ^= *value;
    }
    mutated
}

pub fn docx_package(document: &[u8], parts: &[(&str, &[u8])]) -> Option<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options).ok()?;
    writer
        .write_all(b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>")
        .ok()?;
    writer.start_file("_rels/.rels", options).ok()?;
    writer
        .write_all(b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>")
        .ok()?;
    writer.start_file("word/document.xml", options).ok()?;
    writer.write_all(bounded(document)).ok()?;
    for (path, bytes) in parts {
        writer.start_file(path, options).ok()?;
        writer.write_all(bounded(bytes)).ok()?;
    }
    writer.finish().ok().map(|cursor| cursor.into_inner())
}

pub fn fixed_document_xml() -> &'static [u8] {
    b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>Fuzz anchor</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"
}

pub fn parse_docx_bytes(bytes: Vec<u8>) {
    if let Ok(source) = DocumentSource::from_bytes(bytes) {
        let _ = parse_docx(&source);
    }
}

pub fn laid_out_fixed_document() -> Option<Document> {
    let bytes = docx_package(fixed_document_xml(), &[])?;
    let source = DocumentSource::from_bytes(bytes).ok()?;
    let document = parse_docx(&source).ok()?;
    layout_docx(document).ok().map(|layout| layout.document)
}

pub fn xml_text(data: &[u8]) -> String {
    let mut text = String::new();
    for character in String::from_utf8_lossy(bounded(data)).chars() {
        match character {
            '&' => text.push_str("&amp;"),
            '<' => text.push_str("&lt;"),
            '>' => text.push_str("&gt;"),
            '\t' | '\n' | '\r' => text.push(character),
            value if value >= ' ' => text.push(value),
            _ => {}
        }
    }
    text
}

pub fn paragraph_document(data: &[u8]) -> Vec<u8> {
    format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>{}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>",
        xml_text(data)
    )
    .into_bytes()
}

pub fn pdf_with_content(content: &[u8]) -> Vec<u8> {
    let content = bounded(content);
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        {
            let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
            stream.extend_from_slice(content);
            stream.extend_from_slice(b"\nendstream");
            stream
        },
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

pub fn proof_bundle_with_manifest(manifest: &[u8]) -> Option<Vec<u8>> {
    let source = docx_package(fixed_document_xml(), &[])?;
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    writer.start_file("manifest.json", options).ok()?;
    writer.write_all(bounded(manifest)).ok()?;
    writer.start_file("source.bin", options).ok()?;
    writer.write_all(&source).ok()?;
    writer.finish().ok().map(|cursor| cursor.into_inner())
}
