#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

use docsight_core::{DocsightError, DocumentSource, Rect};
use docsight_pdf::PdfDocument;
use pdf_fixture::{build_pdf, sample_pdf};

#[test]
fn inspects_page_geometry_and_text_spans() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(sample_pdf())?;
    let document = PdfDocument::open(&source)?;
    let info = document.info()?;
    assert_eq!(info.page_count, 1);
    assert_eq!(info.pages[0].width_pt, 200.0);
    assert_eq!(info.pages[0].height_pt, 100.0);
    let page = document.page(1)?;
    assert_eq!(page.number, 1);
    assert!(page.spans.iter().any(|span| span.text == "Hello DOCSIGHT"));
    assert!(page.spans.iter().all(|span| span.confidence == 0.75));
    assert_eq!(page.warnings[0].code, "APPROXIMATED_BASE14_FONT");
    Ok(())
}

#[test]
fn creates_stable_span_identifiers() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(sample_pdf())?;
    let first = PdfDocument::open(&source)?.page(1)?;
    let second = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(first.spans[0].id, second.spans[0].id);
    Ok(())
}

#[test]
fn renders_full_page_and_crop_with_expected_dimensions() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(sample_pdf())?;
    let document = PdfDocument::open(&source)?;
    let page = document.rasterize(1, 144, None)?;
    assert_eq!((page.width_px, page.height_px), (400, 200));
    assert_eq!(&page.png[..8], b"\x89PNG\r\n\x1a\n");
    let crop = document.rasterize(1, 144, Some(Rect::new(10.0, 10.0, 110.0, 60.0)?))?;
    assert_eq!((crop.width_px, crop.height_px), (200, 100));
    assert_eq!(&crop.png[..8], b"\x89PNG\r\n\x1a\n");
    Ok(())
}

#[test]
fn rejects_invalid_page_dpi_and_crop() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(sample_pdf())?;
    let document = PdfDocument::open(&source)?;
    assert!(matches!(
        document.page(2),
        Err(DocsightError::ObjectNotFound { .. })
    ));
    assert!(matches!(
        document.rasterize(1, 35, None),
        Err(DocsightError::InvalidArgument { .. })
    ));
    let outside = Rect::new(0.0, 0.0, 201.0, 100.0)?;
    assert!(matches!(
        document.rasterize(1, 144, Some(outside)),
        Err(DocsightError::InvalidArgument { .. })
    ));
    Ok(())
}

#[test]
fn rejects_malformed_pdf_after_magic_sniffing() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(b"%PDF-1.7\nnot-a-document".to_vec())?;
    assert!(matches!(
        PdfDocument::open(&source),
        Err(DocsightError::MalformedDocument { .. })
    ));
    Ok(())
}

#[test]
fn normalizes_non_zero_media_box_coordinates() -> Result<(), DocsightError> {
    let bytes = build_pdf(
        "BT /F1 10 Tf 30 60 Td (Origin) Tj ET",
        "[10 20 210 120]",
        "",
    );
    let source = DocumentSource::from_bytes(bytes)?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!((page.width_pt, page.height_pt), (200.0, 100.0));
    assert_eq!(page.spans[0].bbox.x0, 20.0);
    assert_eq!(page.spans[0].bbox.y1, 62.0);
    Ok(())
}

#[test]
fn rejects_unsupported_stream_filters_explicitly() -> Result<(), DocsightError> {
    let bytes = build_pdf("BT ET", "[0 0 200 100]", "/Filter /DCTDecode");
    let source = DocumentSource::from_bytes(bytes)?;
    let error = PdfDocument::open(&source)?.page(1);
    assert!(matches!(
        error,
        Err(DocsightError::UnsupportedFeature { feature }) if feature.contains("DCTDecode")
    ));
    Ok(())
}

#[test]
fn decodes_flate_streams_and_exposes_span_provenance() -> Result<(), DocsightError> {
    let content = "BT /F1 12 Tf 20 70 Td (Compressed hello) Tj ET";
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    use std::io::Write as _;
    encoder
        .write_all(content.as_bytes())
        .map_err(|error| DocsightError::MalformedDocument {
            message: error.to_string(),
        })?;
    let compressed = encoder
        .finish()
        .map_err(|error| DocsightError::MalformedDocument {
            message: error.to_string(),
        })?;
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objects.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned());
    objects.push(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
            .to_owned(),
    );
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
    objects.push(format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        compressed.len()
    ));
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        if index == 4 {
            pdf.extend_from_slice(format!("{} 0 obj\n{}", index + 1, object).as_bytes());
            pdf.extend_from_slice(&compressed);
            pdf.extend_from_slice(b"\nendstream\nendobj\n");
        } else {
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
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

    let source = DocumentSource::from_bytes(pdf)?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(page.spans.len(), 1);
    let span = &page.spans[0];
    assert_eq!(span.text, "Compressed hello");
    assert_eq!(span.font_name, "Helvetica");
    assert!(
        (span.baseline_y_pt - 30.0).abs() < 0.001,
        "{}",
        span.baseline_y_pt
    );
    let offset = span.source.offset.unwrap_or(0);
    let length = span.source.length.unwrap_or(0);
    assert!(
        length >= 2,
        "operator anchor length must cover the Tj token"
    );
    let anchored = &content[offset as usize..(offset + length) as usize];
    assert_eq!(anchored, "Tj");
    Ok(())
}

#[test]
fn rejects_encrypted_documents_with_dedicated_error() -> Result<(), DocsightError> {
    let pdf =
        String::from_utf8(sample_pdf()).map_err(|error| DocsightError::MalformedDocument {
            message: error.to_string(),
        })?;
    let encrypted = pdf.replace("/Root 1 0 R >>", "/Root 1 0 R /Encrypt 4 0 R >>");
    let source = DocumentSource::from_bytes(encrypted.into_bytes())?;
    assert!(matches!(
        PdfDocument::open(&source),
        Err(DocsightError::EncryptedDocument)
    ));
    Ok(())
}

#[test]
fn produces_byte_identical_rasters() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(sample_pdf())?;
    let document = PdfDocument::open(&source)?;
    let first = document.rasterize(1, 144, None)?;
    let second = document.rasterize(1, 144, None)?;
    assert_eq!(first.png, second.png);
    Ok(())
}

#[test]
fn executes_initial_text_path_and_transform_operators() -> Result<(), DocsightError> {
    let content = "q 1 0 0 1 0 0 cm 0 0 0 RG 2 w 10 10 m 60 90 140 10 190 90 c S Q BT /F1 10 Tf 1 0 0 1 20 70 Tm [(A) -100 (B)] TJ ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let document = PdfDocument::open(&source)?;
    let page = document.page(1)?;
    assert_eq!(
        page.spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "AB"
    );
    let raster = document.rasterize(1, 144, None)?;
    assert_eq!((raster.width_px, raster.height_px), (400, 200));
    Ok(())
}

#[test]
fn rejects_unknown_content_operators() -> Result<(), DocsightError> {
    let source = DocumentSource::from_bytes(build_pdf("1 unsupported", "[0 0 10 10]", ""))?;
    let error = PdfDocument::open(&source)?.page(1);
    assert!(matches!(
        error,
        Err(DocsightError::UnsupportedFeature { feature }) if feature.contains("unsupported")
    ));
    Ok(())
}
