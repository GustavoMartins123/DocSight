use docsight_core::{DocsightError, DocumentSource, decode_png, encode_png};
use docsight_render::{RenderRequest, RenderTarget, render_document};
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn test_jpeg() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let specification = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("Projeto_DOCSIGHT_Especificacao.docx");
    let mut archive = ZipArchive::new(std::fs::File::open(specification)?)?;
    let mut part = archive.by_name("docProps/thumbnail.jpeg")?;
    let mut bytes = Vec::new();
    part.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn package_with_image(name: &str, bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let document = format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:extent cx="1270000" cy="635000"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#
    );
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/{name}"/></Relationships>"#
    );
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(b"<Types/>")?;
    writer.start_file("word/document.xml", options)?;
    writer.write_all(document.as_bytes())?;
    writer.start_file("word/_rels/document.xml.rels", options)?;
    writer.write_all(rels.as_bytes())?;
    writer.start_file(format!("word/media/{name}"), options)?;
    writer.write_all(bytes)?;
    Ok(writer.finish()?.into_inner())
}

fn render_page(bytes: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(bytes)?;
    let rendered = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;
    Ok(rendered.png().to_vec())
}

fn colour_share(png: &[u8], colour: [u8; 3]) -> Result<f32, DocsightError> {
    let image = decode_png(png)?;
    let total = image.rgba.len() / 4;
    let matching = image
        .rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[0] == colour[0] && pixel[1] == colour[1] && pixel[2] == colour[2])
        .count();
    Ok(matching as f32 / total as f32)
}

fn max_channel_difference(left: &[u8], right: &[u8]) -> Result<u8, DocsightError> {
    let left = decode_png(left)?;
    let right = decode_png(right)?;
    if left.width != right.width || left.height != right.height {
        return Ok(u8::MAX);
    }
    Ok(left
        .rgba
        .iter()
        .zip(right.rgba.iter())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0))
}

#[test]
fn draws_an_embedded_png_into_the_figure_box() -> Result<(), Box<dyn std::error::Error>> {
    let image = encode_png(2, 2, &[0, 128, 255, 0, 128, 255, 0, 128, 255, 0, 128, 255])?;
    let png = render_page(package_with_image("solid.png", &image)?)?;

    let share = colour_share(&png, [0, 128, 255])?;
    let expected = (100.0 * 50.0) / (612.0 * 792.0);
    assert!(
        (share - expected).abs() < expected * 0.15,
        "a 100x50pt figure on a Letter page must cover about {expected} of it, got {share}"
    );
    Ok(())
}

#[test]
fn decodes_an_embedded_jpeg_deterministically() -> Result<(), Box<dyn std::error::Error>> {
    let jpeg = test_jpeg()?;
    let decoded = docsight_core::decode_jpeg(&jpeg)?;
    assert!(decoded.width > 0);
    assert!(decoded.height > 0);
    let width = usize::try_from(decoded.width)?;
    let height = usize::try_from(decoded.height)?;
    assert_eq!(
        decoded.rgba.len(),
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("decoded image size overflow")?
    );

    let package = package_with_image("thumbnail.jpg", &jpeg)?;
    let source = DocumentSource::from_bytes(package.clone())?;
    let request = RenderRequest {
        target: RenderTarget::Page { page: 1 },
        dpi: 72,
    };
    let first = render_document(&source, &request)?;
    let second = render_document(&DocumentSource::from_bytes(package)?, &request)?;

    assert!(
        !first
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
    );
    assert_eq!(first.png(), second.png());
    Ok(())
}

#[test]
fn a_malformed_jpeg_is_reported_as_a_placeholder() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        DocumentSource::from_bytes(package_with_image("broken.jpg", &[0xff, 0xd8, 0xff, 0xe0])?)?;
    let rendered = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;

    assert!(
        rendered
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER")
    );
    Ok(())
}

#[test]
fn rendering_the_same_image_twice_is_byte_identical() -> Result<(), Box<dyn std::error::Error>> {
    let image = encode_png(
        4,
        4,
        &(0..48).map(|value| (value * 5) as u8).collect::<Vec<u8>>(),
    )?;
    let package = package_with_image("noise.png", &image)?;
    let first = render_page(package.clone())?;
    let second = render_page(package)?;

    assert_eq!(first, second);
    assert_eq!(max_channel_difference(&first, &second)?, 0);
    Ok(())
}

#[test]
fn an_undecodable_image_falls_back_to_a_reported_placeholder()
-> Result<(), Box<dyn std::error::Error>> {
    let mut broken = encode_png(2, 2, &[7_u8; 12])?;
    let length = broken.len();
    broken[length - 20] ^= 0xFF;
    let source = DocumentSource::from_bytes(package_with_image("broken.png", &broken)?)?;
    let rendered = render_document(
        &source,
        &RenderRequest {
            target: RenderTarget::Page { page: 1 },
            dpi: 72,
        },
    )?;

    assert!(
        rendered
            .warnings
            .iter()
            .any(|warning| warning.code == "DOCX_FIGURE_RASTER_PLACEHOLDER"),
        "an undecodable image must be reported, not silently skipped"
    );
    Ok(())
}

#[test]
fn identical_documents_render_within_the_declared_tolerance()
-> Result<(), Box<dyn std::error::Error>> {
    const TOLERANCE: u8 = 0;
    let image = encode_png(3, 3, &[200_u8; 27])?;
    let package = package_with_image("flat.png", &image)?;
    let first = render_page(package.clone())?;
    let second = render_page(package)?;
    let difference = max_channel_difference(&first, &second)?;

    assert!(
        difference == TOLERANCE,
        "repeated renders of the same document must stay within {TOLERANCE} per channel, saw {difference}"
    );
    Ok(())
}
