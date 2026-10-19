use docsight_core::{DocsightError, DocumentSource};
use docsight_pdf::PdfDocument;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

const TINY_JPEG: &[u8] = &[
    255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67, 0, 8,
    6, 6, 7, 6, 5, 8, 7, 7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15, 20, 29, 26,
    31, 30, 29, 26, 28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44, 48, 49, 52, 52,
    52, 31, 39, 57, 61, 56, 50, 60, 46, 51, 52, 50, 255, 219, 0, 67, 1, 9, 9, 9, 12, 11, 12, 24,
    13, 13, 24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 255, 192, 0, 17, 8, 0, 2, 0, 2, 3, 1, 34, 0, 2, 17, 1, 3,
    17, 1, 255, 196, 0, 31, 0, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6,
    7, 8, 9, 10, 11, 255, 196, 0, 181, 16, 0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125, 1, 2,
    3, 0, 4, 17, 5, 18, 33, 49, 65, 6, 19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8, 35, 66,
    177, 193, 21, 82, 209, 240, 36, 51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37, 38, 39, 40,
    41, 42, 52, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89,
    90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 131, 132,
    133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165,
    166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198,
    199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 225, 226, 227, 228, 229, 230,
    231, 232, 233, 234, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 255, 196, 0, 31, 1, 0, 3,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 255, 196, 0,
    181, 17, 0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119, 0, 1, 2, 3, 17, 4, 5, 33, 49, 6, 18,
    65, 81, 7, 97, 113, 19, 34, 50, 129, 8, 20, 66, 145, 161, 177, 193, 9, 35, 51, 82, 240, 21, 98,
    114, 209, 10, 22, 36, 52, 225, 37, 241, 23, 24, 25, 26, 38, 39, 40, 41, 42, 53, 54, 55, 56, 57,
    58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99, 100, 101, 102, 103,
    104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 130, 131, 132, 133, 134, 135, 136, 137,
    138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165, 166, 167, 168, 169, 170,
    178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198, 199, 200, 201, 202, 210,
    211, 212, 213, 214, 215, 216, 217, 218, 226, 227, 228, 229, 230, 231, 232, 233, 234, 242, 243,
    244, 245, 246, 247, 248, 249, 250, 255, 218, 0, 12, 3, 1, 0, 2, 17, 3, 17, 0, 63, 0, 226, 232,
    162, 138, 249, 147, 247, 19, 255, 217,
];

fn zlib_compress(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| DocsightError::MalformedDocument {
            message: e.to_string(),
        })?;
    encoder
        .finish()
        .map_err(|e| DocsightError::MalformedDocument {
            message: e.to_string(),
        })
}

fn build_pdf_with_raw_objects(objects: &[(String, Option<Vec<u8>>)]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, (header, stream_data)) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        match stream_data {
            Some(data) => {
                let obj_header = format!("{} 0 obj\n{}\nstream\n", index + 1, header);
                pdf.extend_from_slice(obj_header.as_bytes());
                pdf.extend_from_slice(data);
                pdf.extend_from_slice(b"\nendstream\nendobj\n");
            }
            None => {
                let obj_text = format!("{} 0 obj\n{}\nendobj\n", index + 1, header);
                pdf.extend_from_slice(obj_text.as_bytes());
            }
        }
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

#[test]
fn test_1_valid_jpeg_image_xobject() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>",
                TINY_JPEG.len()
            ),
            Some(TINY_JPEG.to_vec()),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;

    let doc = document.to_document()?;
    let figures = doc.figures().collect::<Vec<_>>();
    assert_eq!(figures.len(), 1);

    let sample_x = 40_usize;
    let sample_y = 50_usize;
    let index = (sample_y * raster.width_px as usize + sample_x) * 3;
    let r = raster.pixels[index];
    let g = raster.pixels[index + 1];
    let b = raster.pixels[index + 2];
    assert!(
        r > 150 && g < 100 && b < 100,
        "sampled pixel should be reddish JPEG: r={r}, g={g}, b={b}"
    );
    Ok(())
}

#[test]
fn test_2_valid_flate_devicergb_8bit() -> Result<(), DocsightError> {
    let raw_rgb = vec![0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255];
    let compressed = zlib_compress(&raw_rgb)?;
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                compressed.len()
            ),
            Some(compressed),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;

    let sample_x = 40_usize;
    let sample_y = 50_usize;
    let index = (sample_y * raster.width_px as usize + sample_x) * 3;
    assert_eq!(
        [
            raster.pixels[index],
            raster.pixels[index + 1],
            raster.pixels[index + 2]
        ],
        [0, 0, 255],
        "sampled pixel should be exact blue"
    );
    Ok(())
}

#[test]
fn test_3_valid_flate_devicegray_8bit() -> Result<(), DocsightError> {
    let raw_gray = vec![120, 120, 120, 120];
    let compressed = zlib_compress(&raw_gray)?;
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                compressed.len()
            ),
            Some(compressed),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;
    let sample_x = 40_usize;
    let sample_y = 50_usize;
    let index = (sample_y * raster.width_px as usize + sample_x) * 3;
    assert_eq!(
        [
            raster.pixels[index],
            raster.pixels[index + 1],
            raster.pixels[index + 2]
        ],
        [120, 120, 120],
        "sampled pixel should be exact gray"
    );
    Ok(())
}

#[test]
fn test_4_malformed_image_fails_closed() -> Result<(), DocsightError> {
    let bad_data = b"not a real jpeg stream at all".to_vec();
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>",
                bad_data.len()
            ),
            Some(bad_data),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let result = document.rasterize(1, 72, None);
    assert!(
        matches!(result, Err(DocsightError::MalformedDocument { .. })),
        "malformed image must fail closed"
    );
    Ok(())
}

#[test]
fn test_5_excessive_dimensions_fail_closed() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            "<< /Type /XObject /Subtype /Image /Width 999999 /Height 999999 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 4 >>".to_owned(),
            Some(vec![0, 0, 0, 0]),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let result = document.rasterize(1, 72, None);
    assert!(
        matches!(result, Err(DocsightError::ResourceLimit { .. })),
        "excessive image dimensions must fail closed"
    );
    Ok(())
}

#[test]
fn test_6_unsupported_filter_fails_closed() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FooDecode /Length 4 >>".to_owned(),
            Some(vec![0, 0, 0, 0]),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let result = document.rasterize(1, 72, None);
    assert!(
        matches!(result, Err(DocsightError::UnsupportedFeature { .. })),
        "unsupported image filter must fail closed"
    );
    Ok(())
}

#[test]
fn test_7_repeated_image_rendering_is_byte_identical() -> Result<(), DocsightError> {
    let raw_rgb = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
    let compressed = zlib_compress(&raw_rgb)?;
    let content = "q 50 0 0 50 25 25 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                compressed.len()
            ),
            Some(compressed),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let first = document.rasterize(1, 144, None)?;
    let second = document.rasterize(1, 144, None)?;

    assert_eq!(first.png, second.png);
    assert_eq!(first.pixels, second.pixels);
    Ok(())
}

#[test]
fn test_8_image_respects_page_transformation_ctm() -> Result<(), DocsightError> {
    let raw_rgb = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
    let compressed = zlib_compress(&raw_rgb)?;
    let content = "q 0 30 -30 0 70 20 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                compressed.len()
            ),
            Some(compressed),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;

    let outside_index = (10_usize * raster.width_px as usize + 10_usize) * 3;
    assert_eq!(
        [
            raster.pixels[outside_index],
            raster.pixels[outside_index + 1],
            raster.pixels[outside_index + 2]
        ],
        [255, 255, 255],
        "pixel outside rotated image should be background white"
    );

    let inside_index = (60_usize * raster.width_px as usize + 55_usize) * 3;
    assert_eq!(
        [
            raster.pixels[inside_index],
            raster.pixels[inside_index + 1],
            raster.pixels[inside_index + 2]
        ],
        [255, 0, 0],
        "pixel inside rotated image should be red"
    );
    Ok(())
}

#[test]
fn test_9_unsupported_color_space_fails_closed() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace [/Separation /Spot /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >>] /BitsPerComponent 8 /Length 2 >>".to_owned(),
            Some(vec![0, 0]),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let result = document.rasterize(1, 72, None);
    assert!(
        matches!(result, Err(DocsightError::UnsupportedFeature { .. })),
        "unsupported color space must fail closed"
    );
    Ok(())
}

#[test]
fn test_10_supported_image_does_not_produce_placeholder() -> Result<(), DocsightError> {
    let raw_rgb = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
    let compressed = zlib_compress(&raw_rgb)?;
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                compressed.len()
            ),
            Some(compressed),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;
    assert!(
        raster
            .pixels
            .chunks_exact(3)
            .any(|pixel| pixel != [255, 255, 255]),
        "a supported image must be painted, not replaced by a placeholder"
    );

    Ok(())
}

fn ascii85_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(4) {
        let mut group = [0_u8; 4];
        group[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes(group);
        if chunk.len() == 4 && value == 0 {
            out.push(b'z');
            continue;
        }
        let mut symbols = [0_u8; 5];
        let mut remaining = value;
        for slot in symbols.iter_mut().rev() {
            *slot = (remaining % 85) as u8 + b'!';
            remaining /= 85;
        }
        out.extend_from_slice(&symbols[..chunk.len() + 1]);
    }
    out.extend_from_slice(b"~>");
    out
}

#[test]
fn test_11_ascii85_flate_filter_array_decodes() -> Result<(), DocsightError> {
    let raw_rgb = vec![200, 10, 10, 200, 10, 10, 200, 10, 10, 200, 10, 10];
    let compressed = zlib_compress(&raw_rgb)?;
    let mut wrapped = ascii85_encode(&compressed);
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter [/ASCII85Decode /FlateDecode] /Length {} >>",
                wrapped.len()
            ),
            Some(std::mem::take(&mut wrapped)),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;

    let sample_x = 40_usize;
    let sample_y = 50_usize;
    let index = (sample_y * raster.width_px as usize + sample_x) * 3;
    assert!(
        raster.pixels[index] > 150 && raster.pixels[index + 1] < 60,
        "sampled pixel should decode to the red image"
    );
    Ok(())
}

#[test]
fn test_12_smask_alpha_composites_over_page() -> Result<(), DocsightError> {
    let raw_rgb = vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0];
    let compressed = zlib_compress(&raw_rgb)?;
    let mask_raw = vec![128_u8, 128, 128, 128];
    let mask_compressed = zlib_compress(&mask_raw)?;
    let mut wrapped = ascii85_encode(&compressed);
    let mut wrapped_mask = ascii85_encode(&mask_compressed);
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 6 0 R >>".to_owned(),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter [/ASCII85Decode /FlateDecode] /SMask 5 0 R /Length {} >>",
                wrapped.len()
            ),
            Some(std::mem::take(&mut wrapped)),
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter [/ASCII85Decode /FlateDecode] /Length {} >>",
                wrapped_mask.len()
            ),
            Some(std::mem::take(&mut wrapped_mask)),
        ),
        (
            format!("<< /Length {} >>", content.len()),
            Some(content),
        ),
    ];
    let pdf_bytes = build_pdf_with_raw_objects(&objects);
    let source = DocumentSource::from_bytes(pdf_bytes)?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;

    let sample_x = 40_usize;
    let sample_y = 50_usize;
    let index = (sample_y * raster.width_px as usize + sample_x) * 3;
    let r = raster.pixels[index];
    let g = raster.pixels[index + 1];
    assert!(
        r > 240 && g > 90 && g < 170,
        "SMask alpha must blend red image over white page: r={r} g={g}"
    );
    Ok(())
}

fn raster_image_pixels(
    fields: &str,
    samples: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, DocsightError> {
    let content = format!("q {width} 0 0 {height} 0 0 cm /Im0 Do Q").into_bytes();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"
            ),
            None,
        ),
        (
            format!(
                "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} {fields} /Length {} >>",
                samples.len()
            ),
            Some(samples.to_vec()),
        ),
        (format!("<< /Length {} >>", content.len()), Some(content)),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_raw_objects(&objects))?;
    let document = PdfDocument::open(&source)?;
    let first = document.rasterize(1, 72, None)?;
    assert_eq!((first.width_px, first.height_px), (width, height));
    assert_eq!(first.pixels, document.rasterize(1, 72, None)?.pixels);
    Ok(first.pixels)
}

#[test]
fn indexed_images_preserve_raw_indices_at_every_supported_bit_depth() -> Result<(), DocsightError> {
    for (bits, samples) in [
        (1, vec![0b0100_0000]),
        (2, vec![0b0001_1000]),
        (4, vec![0x01, 0x20]),
        (8, vec![0, 1, 2]),
        (16, vec![0, 0, 0, 1, 0, 2]),
    ] {
        let high = if bits == 1 { 1 } else { 2 };
        let fields = format!(
            "/ColorSpace [/Indexed /DeviceRGB {high} <ff000000ff000000ff>] /BitsPerComponent {bits}"
        );
        let pixels = raster_image_pixels(&fields, &samples, 3, 1)?;
        let last = if bits == 1 { [255, 0, 0] } else { [0, 0, 255] };
        assert_eq!(
            pixels,
            [vec![255, 0, 0, 0, 255, 0], last.to_vec()].concat(),
            "bits={bits}"
        );
    }
    Ok(())
}

#[test]
fn indexed_images_apply_explicit_decode_before_palette_lookup() -> Result<(), DocsightError> {
    let fields = "/ColorSpace [/Indexed /DeviceRGB 2 <ff000000ff000000ff>] /BitsPerComponent 8 /Decode [2 0]";
    assert_eq!(
        raster_image_pixels(fields, &[0, 128, 255], 3, 1)?,
        [0, 0, 255, 0, 255, 0, 255, 0, 0]
    );
    Ok(())
}

#[test]
fn indexed_images_clamp_indices_to_palette_high() -> Result<(), DocsightError> {
    let fields = "/ColorSpace [/Indexed /DeviceRGB 2 <ff000000ff000000ff>] /BitsPerComponent 8";
    assert_eq!(
        raster_image_pixels(fields, &[2, 3, 255], 3, 1)?,
        [0, 0, 255].repeat(3)
    );
    Ok(())
}

#[test]
fn indexed_subbyte_images_preserve_row_padding() -> Result<(), DocsightError> {
    let fields = "/ColorSpace [/Indexed /DeviceRGB 2 <ff000000ff000000ff>] /BitsPerComponent 2";
    assert_eq!(
        raster_image_pixels(fields, &[0x60, 0x90], 3, 2)?,
        [
            0, 255, 0, 0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 255, 0, 255, 0, 0
        ]
    );
    Ok(())
}

#[test]
fn color_key_masks_use_interleaved_component_ranges() -> Result<(), DocsightError> {
    let fields = "/ColorSpace /DeviceRGB /BitsPerComponent 8 /Mask [255 255 0 0 0 0]";
    assert_eq!(
        raster_image_pixels(fields, &[255, 0, 0, 254, 0, 0, 255, 1, 0], 3, 1)?,
        [255, 255, 255, 254, 0, 0, 255, 1, 0]
    );
    Ok(())
}

#[test]
fn color_key_masks_compare_raw_samples_before_decode() -> Result<(), DocsightError> {
    let fields = "/ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0] /Mask [0 0]";
    assert_eq!(
        raster_image_pixels(fields, &[0, 255], 2, 1)?,
        [255, 255, 255, 0, 0, 0]
    );
    Ok(())
}

#[test]
fn color_key_masks_preserve_sixteen_bit_sample_precision() -> Result<(), DocsightError> {
    let fields = "/ColorSpace /DeviceGray /BitsPerComponent 16 /Mask [4097 4097]";
    assert_eq!(
        raster_image_pixels(fields, &[0x10, 0x01, 0x10, 0x02], 2, 1)?,
        [255, 255, 255, 16, 16, 16]
    );
    Ok(())
}

#[test]
fn color_key_masks_use_unscaled_subbyte_samples() -> Result<(), DocsightError> {
    for (bits, samples) in [
        (1, vec![0b0100_0000]),
        (2, vec![0b0001_0000]),
        (4, vec![0x01]),
    ] {
        let fields = format!("/ColorSpace /DeviceGray /BitsPerComponent {bits} /Mask [1 1]");
        assert_eq!(
            raster_image_pixels(&fields, &samples, 2, 1)?,
            [0, 0, 0, 255, 255, 255],
            "bits={bits}"
        );
    }
    Ok(())
}

#[test]
fn indexed_color_key_masks_compare_original_indices() -> Result<(), DocsightError> {
    let fields = "/ColorSpace [/Indexed /DeviceRGB 2 <ff000000ff000000ff>] /BitsPerComponent 8 /Decode [2 0] /Mask [0 0]";
    assert_eq!(
        raster_image_pixels(fields, &[0, 255], 2, 1)?,
        [255, 255, 255, 255, 0, 0]
    );
    Ok(())
}

#[test]
fn color_key_masks_include_both_range_boundaries() -> Result<(), DocsightError> {
    let fields = "/ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [128 129]";
    assert_eq!(
        raster_image_pixels(fields, &[127, 128, 129, 130], 4, 1)?,
        [127, 127, 127, 255, 255, 255, 255, 255, 255, 130, 130, 130]
    );
    Ok(())
}

#[test]
fn malformed_color_key_masks_fail_closed() {
    for mask in ["[-1 0]", "[0 256]", "[2 1]", "[0.0 1]", "[0]", "[0 1 0 1]"] {
        let fields = format!("/ColorSpace /DeviceGray /BitsPerComponent 8 /Mask {mask}");
        assert!(
            matches!(
                raster_image_pixels(&fields, &[0], 1, 1),
                Err(DocsightError::MalformedDocument { .. })
            ),
            "mask={mask}"
        );
    }
    let fields = "/ColorSpace /DeviceGray /BitsPerComponent 1 /Mask [0 2]";
    assert!(matches!(
        raster_image_pixels(fields, &[0], 1, 1),
        Err(DocsightError::MalformedDocument { .. })
    ));
}

#[test]
fn jpx_alpha_does_not_shift_color_channels() -> Result<(), DocsightError> {
    let fields = "/Filter /JPXDecode /ColorSpace /DeviceRGB /SMaskInData 1";
    assert_eq!(
        raster_image_pixels(fields, include_bytes!("fixtures/rgba.jp2"), 3, 1)?,
        [255, 0, 0, 127, 255, 127, 255, 255, 255]
    );
    Ok(())
}

#[test]
fn jpx_cmyk_preserves_white_colors_and_midtone_channels() -> Result<(), DocsightError> {
    let fields = "/Filter /JPXDecode /ColorSpace /DeviceCMYK";
    assert_eq!(
        raster_image_pixels(fields, include_bytes!("fixtures/cmyk.jp2"), 5, 1)?,
        [
            255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 95, 63, 31
        ]
    );
    Ok(())
}
