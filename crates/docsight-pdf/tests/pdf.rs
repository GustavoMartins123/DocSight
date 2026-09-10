#[path = "../../../fixtures/pdf_fixture.rs"]
mod pdf_fixture;

use docsight_core::{DocsightError, DocumentSource, Rect};
use docsight_pdf::{
    PdfDocument, PdfTraceDisplayOperation, PdfTraceLineCap, PdfTraceLineJoin, PdfTraceResourceKind,
};
use pdf_fixture::{build_pdf, sample_pdf};

#[test]
fn reconstructs_all_synthetic_measurements() -> Result<(), Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(
        include_bytes!("../../../fixtures/validation/synthetic_table.pdf").to_vec(),
    )?;
    let document = PdfDocument::open(&source)?.to_document()?;
    let tables = document.tables().collect::<Vec<_>>();
    assert_eq!(tables.len(), 1);
    let (block, table) = tables[0];
    assert_eq!(block.id.as_str(), "tbl_b54dbb45da2a78dec493d24c309ea8c0");
    assert_eq!((table.rows, table.columns), (25, 12));
    assert_eq!(table.detector.as_deref(), Some("ruled"));
    let expected = [
        "3,8 10,0 8,0 9,0 2,9 14,3",
        "18,0 10,0 5,2 4,6 13,8 13,0",
        "5,9 7,0 10,5 14,7 4,7 2,9",
        "9,0 5,7 17,0 3,8 13,0 14,9",
        "6,1 13,2 14,8 13,8 18,4 5,9",
        "8,2 15,2 17,4 8,2 13,5 17,2",
        "18,1 4,0 7,0 16,8 18,3 18,4",
        "5,9 15,5 15,8 10,5 6,2 7,5",
        "10,0 9,0 7,5 12,5 4,0 15,5",
        "3,8 2,8 13,8 12,8 10,5 4,3",
        "17,1 3,0 4,5 6,2 7,7 7,9",
        "15,0 4,3 17,0 15,5 13,5 16,6",
        "16,6 3,5 8,8 17,6 15,4 8,0",
        "14,9 5,7 18,1 17,7 5,5 13,5",
        "3,8 16,0 3,3 18,3 16,0 13,0",
        "5,8 16,6 7,0 11,4 8,8 7,4",
        "13,0 15,2 16,6 15,8 18,6 6,4",
        "8,8 15,0 17,8 3,8 16,4 17,4",
        "5,7 7,6 8,6 9,0 11,8 14,3",
        "17,0 13,2 4,5 17,6 3,4 18,2",
    ];
    for (row, values) in expected.iter().enumerate() {
        for (column, expected) in values.split_whitespace().enumerate() {
            let cells = table
                .cells
                .iter()
                .filter(|cell| cell.row as usize == row + 3 && cell.column as usize == column + 1)
                .collect::<Vec<_>>();
            assert_eq!(cells.len(), 1);
            assert_eq!(cells[0].text, expected, "row {row}, column {column}");
            assert_eq!((cells[0].row_span, cells[0].column_span), (1, 1));
        }
    }
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "EVENT A: 05/09" && cell.column_span == 2)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.row == 1 && cell.column == 0 && cell.text == "GRUPO:")
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.row == 2 && cell.column == 1 && cell.text == "A1")
    );
    assert!(
        !table
            .cells
            .iter()
            .any(|cell| cell.text.contains("DATA DE") || cell.text.contains("1_P.A"))
    );
    Ok(())
}

fn build_pdf_with_objects(objects: &[String]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
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
    assert_eq!(page.warnings[0].code, "APPROXIMATED_PDF_FONT");
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
fn composes_page_and_text_matrices_in_pdf_order() -> Result<(), DocsightError> {
    let content = "0.5 0 0 -0.5 0 100 cm BT /F1 10 Tf 20 20 Td (A) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    let span = &page.spans[0];
    assert!((span.bbox.x0 - 10.0).abs() < 0.001);
    assert!((span.bbox.y0 - 9.0).abs() < 0.001);
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

#[test]
fn decodes_simple_truetype_text_through_to_unicode() -> Result<(), DocsightError> {
    let content = "BT /F1 12 Tf 20 70 Td (\\001\\002) Tj ET";
    let cmap = "begincmap\n2 beginbfchar\n<01> <00E9>\n<02> <4F60>\nendbfchar\nendcmap";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Example-Bold /ToUnicode 6 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(page.spans[0].text, "é你");
    assert_eq!(page.spans[0].font_name, "Example-Bold");
    assert!(page.spans[0].bold);
    assert_eq!(page.warnings[0].code, "APPROXIMATED_PDF_FONT");
    Ok(())
}

#[test]
fn preserves_clipping_paths_in_native_rasterization() -> Result<(), DocsightError> {
    let content = "0 0 100 100 re W n 0 0 200 100 re f";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;
    let inside = (50usize * raster.width_px as usize + 50) * 3;
    let outside = (50usize * raster.width_px as usize + 150) * 3;
    assert_eq!(&raster.pixels[inside..inside + 3], &[0, 0, 0]);
    assert_eq!(&raster.pixels[outside..outside + 3], &[255, 255, 255]);
    Ok(())
}

#[test]
fn applies_ext_graphics_state_alpha_without_approximation() -> Result<(), DocsightError> {
    let content = "q /GS0 gs 1 0 0 rg 10 10 80 80 re f Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ExtGState << /GS0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /ExtGState /ca 0.5 /CA 0.5 >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let document = PdfDocument::open(&source)?;
    let page = document.page(1)?;
    assert!(page.warnings.is_empty());
    assert_eq!(page.spans.len(), 0);
    let raster = document.rasterize(1, 72, None)?;
    assert!(raster.warnings.is_empty());
    let pixel = (50usize * raster.width_px as usize + 50) * 3;
    assert_eq!(&raster.pixels[pixel..pixel + 3], &[255, 128, 128]);
    let trace = document.trace_page(1)?;
    assert!(trace.resources.iter().any(|resource| {
        resource.name == "GS0" && resource.kind == PdfTraceResourceKind::GraphicsState
    }));
    assert!(trace.operations.iter().any(|operation| matches!(
        operation,
        PdfTraceDisplayOperation::Fill {
            color_argb: 0x80ff_0000,
            ..
        }
    )));
    Ok(())
}

#[test]
fn renders_dash_cap_join_and_miter_styles_without_warning() -> Result<(), DocsightError> {
    let content = "q /GS0 gs 10 50 m 90 50 l S Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ExtGState << /GS0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /ExtGState /LW 4 /LC 0 /LJ 2 /ML 5 /D [[4 4] 0] >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let document = PdfDocument::open(&source)?;
    let raster = document.rasterize(1, 72, None)?;
    assert!(raster.warnings.is_empty());
    let painted = (50usize * raster.width_px as usize + 12) * 3;
    let gap = (50usize * raster.width_px as usize + 15) * 3;
    assert_eq!(&raster.pixels[painted..painted + 3], &[0, 0, 0]);
    assert_eq!(&raster.pixels[gap..gap + 3], &[255, 255, 255]);
    let trace = document.trace_page(1)?;
    assert!(trace.operations.iter().any(|operation| matches!(
        operation,
        PdfTraceDisplayOperation::Stroke {
            width_pt: 4.0,
            line_cap: PdfTraceLineCap::Butt,
            line_join: PdfTraceLineJoin::Bevel,
            miter_limit: 5.0,
            dash_pattern_pt,
            dash_phase_pt: 0.0,
            ..
        } if dash_pattern_pt == &[4.0, 4.0]
    )));
    Ok(())
}

#[test]
fn rejects_non_normal_blend_modes() -> Result<(), DocsightError> {
    let content = "/GS0 gs 10 10 80 80 re f";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /ExtGState << /GS0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /ExtGState /BM /Multiply >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let error = PdfDocument::open(&source)?.rasterize(1, 72, None);
    assert!(matches!(
        error,
        Err(DocsightError::UnsupportedFeature { feature }) if feature == "PDF blend mode Multiply"
    ));
    Ok(())
}

#[test]
fn marks_text_spans_with_active_clipping_paths() -> Result<(), DocsightError> {
    let content = "0 0 100 100 re W n BT /F1 10 Tf 150 50 Td (Clipped) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert!(page.spans[0].clipped);
    Ok(())
}

#[test]
fn decodes_type0_font_through_to_unicode() -> Result<(), DocsightError> {
    let content = "BT /F1 12 Tf 20 70 Td <00010002> Tj ET";
    let cmap = "begincmap\n2 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar\nendcmap";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 7 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Example-Bold /Encoding /Identity-H /DescendantFonts [5 0 R] /ToUnicode 6 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+Example-Bold /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(page.spans[0].text, "AB");
    assert_eq!(page.spans[0].font_name, "Example-Bold");
    assert!(page.spans[0].bold);
    Ok(())
}

#[test]
fn exposes_xobject_placements_as_figure_blocks() -> Result<(), DocsightError> {
    let content = "q 100 0 0 50 10 20 cm /Im#30 Do Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> /XObject << /Im0 5 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\nabc\nendstream".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let document = PdfDocument::open(&source)?.to_document()?;
    let figures = document.figures().collect::<Vec<_>>();
    assert_eq!(figures.len(), 1);
    let block = figures[0].0;
    assert_eq!(block.page, Some(1));
    assert_eq!(block.bbox, Some(Rect::new(10.0, 30.0, 110.0, 80.0)?));
    assert_eq!(figures[0].1.resource_id.as_deref(), Some("Im0"));
    assert!(
        document.warnings.iter().any(|warning| {
            warning.code == "PDF_XOBJECT_PLACEHOLDER" && warning.page == Some(1)
        })
    );
    Ok(())
}

#[test]
fn renders_explicit_placeholder_for_undecoded_xobjects() -> Result<(), DocsightError> {
    let content = "q 100 0 0 50 10 20 cm /Im0 Do Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\nabc\nendstream".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let raster = PdfDocument::open(&source)?.rasterize(1, 72, None)?;
    assert!(
        raster
            .pixels
            .chunks_exact(3)
            .any(|pixel| pixel != [255, 255, 255])
    );
    assert!(
        raster.warnings.iter().any(|warning| {
            warning.code == "PDF_XOBJECT_PLACEHOLDER" && warning.page == Some(1)
        })
    );
    Ok(())
}

#[test]
fn exposes_inline_images_as_explicit_figure_placeholders() -> Result<(), DocsightError> {
    let content = "q 100 0 0 50 10 20 cm BI /W 1 /H 1 /BPC 8 /CS /RGB ID abc EI Q";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let document = PdfDocument::open(&source)?.to_document()?;
    let figures = document.figures().collect::<Vec<_>>();
    assert_eq!(figures.len(), 1);
    assert_eq!(figures[0].1.resource_id.as_deref(), Some("<inline-image>"));
    assert!(
        document.warnings.iter().any(|warning| {
            warning.code == "PDF_XOBJECT_PLACEHOLDER" && warning.page == Some(1)
        })
    );
    Ok(())
}

#[test]
fn ignores_empty_text_shows_without_invalid_geometry() -> Result<(), DocsightError> {
    let content = "BT /F1 10 Tf 20 70 Td () Tj (Hi) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(page.spans.len(), 1);
    assert_eq!(page.spans[0].text, "Hi");
    let content = "BT /F1 10 Tf 20 70 Td [() (A)] TJ ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(
        page.spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>(),
        "A"
    );
    Ok(())
}

#[test]
fn accepts_negative_and_zero_horizontal_scale() -> Result<(), DocsightError> {
    let content = "BT /F1 10 Tf -100 Tz 20 70 Td (Mirror) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert_eq!(page.spans.len(), 1);
    assert_eq!(page.spans[0].text, "Mirror");
    assert!(page.spans[0].bbox.x1 > page.spans[0].bbox.x0);
    let content = "BT /F1 10 Tf 0 Tz 20 70 Td (Hi) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let page = PdfDocument::open(&source)?.page(1)?;
    assert!(page.spans.is_empty());
    Ok(())
}

#[test]
fn accepts_invisible_text_rendering_modes_for_extraction() -> Result<(), DocsightError> {
    for mode in [0, 1, 2, 3] {
        let content = format!("BT /F1 10 Tf {mode} Tr 20 70 Td (OCR) Tj ET");
        let source = DocumentSource::from_bytes(build_pdf(&content, "[0 0 200 100]", ""))?;
        let document = PdfDocument::open(&source)?.to_document()?;
        assert_eq!(document.paragraphs().count(), 1, "mode {mode}");
        assert!(
            !document
                .warnings
                .iter()
                .any(|warning| warning.code == "PDF_CLIP_TEXT_VISUAL"),
            "mode {mode} must not emit a clip warning"
        );
    }
    let content = "BT /F1 10 Tf 5 Tr 20 70 Td (Clip) Tj ET";
    let source = DocumentSource::from_bytes(build_pdf(content, "[0 0 200 100]", ""))?;
    let document = PdfDocument::open(&source)?.to_document()?;
    assert_eq!(document.paragraphs().count(), 1);
    assert!(
        document
            .warnings
            .iter()
            .any(|warning| warning.code == "PDF_CLIP_TEXT_VISUAL" && warning.page == Some(1))
    );
    Ok(())
}

#[test]
fn accepts_raster_only_extgstate_entries_with_visual_warning() -> Result<(), DocsightError> {
    let content = "q /GS0 gs BT /F1 10 Tf 20 70 Td (Hello) Tj ET Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> /ExtGState << /GS0 4 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /ExtGState /SA true >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let document = PdfDocument::open(&source)?.to_document()?;
    assert_eq!(document.paragraphs().count(), 1);
    assert!(document.warnings.iter().any(|warning| {
        warning.code == "PDF_EXTGSTATE_IGNORED"
            && warning.page == Some(1)
            && warning.message.contains("SA")
    }));
    Ok(())
}

#[test]
fn accepts_unknown_extgstate_entries_with_visual_warning() -> Result<(), DocsightError> {
    let content = "q /GS0 gs BT /F1 10 Tf 20 70 Td (Hello) Tj ET Q";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> /ExtGState << /GS0 4 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /ExtGState /FutureKey 42 >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let source = DocumentSource::from_bytes(build_pdf_with_objects(&objects))?;
    let document = PdfDocument::open(&source)?.to_document()?;
    assert_eq!(document.paragraphs().count(), 1);
    assert!(document.warnings.iter().any(|warning| {
        warning.code == "PDF_EXTGSTATE_IGNORED"
            && warning.page == Some(1)
            && warning.message.contains("FutureKey")
    }));
    Ok(())
}
