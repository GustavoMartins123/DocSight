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

    assert!(
        !raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER"),
        "valid JPEG should not produce placeholder warning"
    );

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

    assert!(
        !raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER")
    );

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

    assert!(
        !raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER")
    );

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
fn test_4_malformed_image_falls_back_to_placeholder() -> Result<(), DocsightError> {
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
    let raster = document.rasterize(1, 72, None)?;

    assert!(
        raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER"),
        "malformed image must emit placeholder warning"
    );
    Ok(())
}

#[test]
fn test_5_excessive_dimensions_falls_back_to_placeholder() -> Result<(), DocsightError> {
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
    let raster = document.rasterize(1, 72, None)?;

    assert!(
        raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER"),
        "excessive dimension must fall back to placeholder"
    );
    Ok(())
}

#[test]
fn test_6_unsupported_filter_falls_back_to_placeholder() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JBIG2Decode /Length 4 >>".to_owned(),
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
    let raster = document.rasterize(1, 72, None)?;

    assert!(
        raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER"),
        "unsupported filter must produce placeholder warning"
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
fn test_9_fallback_produces_explicit_diagnostic() -> Result<(), DocsightError> {
    let content = "q 60 0 0 40 20 30 cm /Im0 Do Q".as_bytes().to_vec();
    let objects = vec![
        ("<< /Type /Catalog /Pages 2 0 R >>".to_owned(), None),
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), None),
        (
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            None,
        ),
        (
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceCMYK /BitsPerComponent 8 /Length 8 >>".to_owned(),
            Some(vec![0; 8]),
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

    let placeholder_warning = raster
        .warnings
        .iter()
        .find(|w| w.code == "PDF_XOBJECT_PLACEHOLDER");
    assert!(placeholder_warning.is_some());
    let w = placeholder_warning.ok_or_else(|| DocsightError::MalformedDocument {
        message: "expected placeholder warning".to_owned(),
    })?;
    assert_eq!(w.severity, docsight_core::DiagnosticSeverity::Warning);
    assert!(!w.effect.is_empty());
    assert_eq!(w.page, Some(1));
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
        !raster
            .warnings
            .iter()
            .any(|w| w.code == "PDF_XOBJECT_PLACEHOLDER"),
        "supported image must not produce PDF_XOBJECT_PLACEHOLDER"
    );
    Ok(())
}
