use docsight_core::DocumentSource;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_worker::{SandboxPolicy, find_worker_binary, run_in_sandbox};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        out.push((state >> 33) as u8);
    }
    out
}

fn package(
    document_xml: &str,
    styles_xml: Option<&str>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", options)?;
        zip.write_all(b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>")?;
        zip.start_file("_rels/.rels", options)?;
        zip.write_all(b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>")?;
        zip.start_file("word/document.xml", options)?;
        zip.write_all(document_xml.as_bytes())?;
        if let Some(styles) = styles_xml {
            zip.start_file("word/styles.xml", options)?;
            zip.write_all(styles.as_bytes())?;
            zip.start_file("word/_rels/document.xml.rels", options)?;
            zip.write_all(b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>")?;
        }
        zip.finish()?;
    }
    Ok(cursor.into_inner())
}

fn package_with_traversal(name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", options)?;
        zip.write_all(b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>")?;
        zip.start_file("_rels/.rels", options)?;
        zip.write_all(b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>")?;
        zip.start_file("word/document.xml", options)?;
        zip.write_all(b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p/></w:body></w:document>")?;
        zip.start_file(name, options)?;
        zip.write_all(b"hostile")?;
        zip.finish()?;
    }
    Ok(cursor.into_inner())
}

fn build_pdf(content: &str, media_box: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ),
    ];
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
fn fuzz_opc_container_rejects_arbitrary_garbage() {
    for seed in 1..=20 {
        let bytes = pseudo_random_bytes(seed, 1024);
        if let Ok(source) = DocumentSource::from_bytes(bytes) {
            let res = parse_docx(&source);
            assert!(res.is_err());
        }
    }
}

#[test]
fn fuzz_pdf_syntax_rejects_corrupted_headers_and_garbage() {
    for seed in 100..=120 {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend_from_slice(&pseudo_random_bytes(seed, 2048));
        if let Ok(source) = DocumentSource::from_bytes(bytes) {
            let res = PdfDocument::open(&source);
            assert!(res.is_err());
        }
    }
}

#[test]
fn fuzz_pdf_xref_rejects_corrupted_tables() -> Result<(), Box<dyn std::error::Error>> {
    let bad_xref = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Root 1 0 R >>\nstartxref\n999999\n%%EOF";
    let source = DocumentSource::from_bytes(bad_xref.to_vec())?;
    let res = PdfDocument::open(&source);
    assert!(res.is_err());
    Ok(())
}

#[test]
fn fuzz_pdf_content_stream_rejects_malformed_operators() -> Result<(), Box<dyn std::error::Error>> {
    let bad_pdf = build_pdf("BT /F1 12 Tf (hello) UNKNOWN_OP ET", "[0 0 200 200]");
    let source = DocumentSource::from_bytes(bad_pdf)?;
    let doc = PdfDocument::open(&source)?;
    let page_res = doc.page(1);
    assert!(page_res.is_err());
    Ok(())
}

#[test]
fn fuzz_styles_cascade_cycle_detection() -> Result<(), Box<dyn std::error::Error>> {
    let doc_xml = "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:pPr><w:pStyle w:val=\"A\"/></w:pPr><w:r><w:t>Cycle</w:t></w:r></w:p></w:body></w:document>";
    let styles_xml = "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:style w:type=\"paragraph\" w:styleId=\"A\"><w:basedOn w:val=\"B\"/></w:style><w:style w:type=\"paragraph\" w:styleId=\"B\"><w:basedOn w:val=\"A\"/></w:style></w:styles>";
    let bytes = package(doc_xml, Some(styles_xml))?;
    let source = DocumentSource::from_bytes(bytes)?;
    let res = parse_docx(&source);
    assert!(res.is_err());
    Ok(())
}

#[test]
fn fuzz_ooxml_relationships_rejects_path_traversal() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = package_with_traversal("../outside.xml")?;
    let source = DocumentSource::from_bytes(bytes)?;
    let res = parse_docx(&source);
    assert!(res.is_err());
    Ok(())
}

#[test]
fn sandbox_worker_inspect_runs_isolated_process() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight()
        .args(["inspect", doc_str, "--sandbox", "--json"])
        .output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["schema"], "docsight.agent/v2");
    assert_eq!(value["result"]["format"], "docx");
    assert_eq!(value["result"]["pages"], 4);

    Ok(())
}

#[test]
fn sandbox_worker_failure_maps_to_backend_failure_exit_code_30()
-> Result<(), Box<dyn std::error::Error>> {
    let worker_exe = find_worker_binary()?;
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let policy = SandboxPolicy::default();
    let res = run_in_sandbox(
        Some(&worker_exe),
        &policy,
        &[
            "--crash-for-test".to_owned(),
            "inspect".to_owned(),
            doc_str.to_owned(),
        ],
    );
    assert!(res.is_err());
    if let Err(err) = res {
        assert_eq!(err.exit_code(), 30);
    }

    Ok(())
}

#[test]
fn fingerprint_command_matches_specification_contract() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;

    let output = docsight().args(["fingerprint", doc_str]).output()?;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains("file_sha256"));
    assert!(text.contains("engine"));
    assert!(text.contains("ooxml_engine"));
    assert!(text.contains("pdf_engine"));
    assert!(text.contains("raster_engine"));
    assert!(text.contains("fonts"));
    assert!(text.contains("layout_profile"));
    assert!(text.contains("result_fingerprint"));

    let json_out = docsight()
        .args(["fingerprint", doc_str, "--json"])
        .output()?;
    assert!(json_out.status.success());
    let val: serde_json::Value = serde_json::from_slice(&json_out.stdout)?;
    assert_eq!(val["schema"], "docsight.agent/v2");
    let result = &val["result"];
    assert!(!result["file_sha256"].as_str().ok_or("missing")?.is_empty());
    assert!(
        !result["result_fingerprint"]
            .as_str()
            .ok_or("missing")?
            .is_empty()
    );

    Ok(())
}

#[test]
fn fuzz_numbering_rejects_broken_definitions() -> Result<(), Box<dyn std::error::Error>> {
    let doc_xml = "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:pPr><w:numPr><w:numId w:val=\"7\"/><w:ilvl w:val=\"0\"/></w:numPr></w:pPr><w:r><w:t>Item</w:t></w:r></w:p></w:body></w:document>";
    let numbering_xml = "<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"></w:lvl></w:abstractNum><w:num w:numId=\"7\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>";
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", options)?;
        zip.write_all(b"<Types/>")?;
        zip.start_file("word/document.xml", options)?;
        zip.write_all(doc_xml.as_bytes())?;
        zip.start_file("word/numbering.xml", options)?;
        zip.write_all(numbering_xml.as_bytes())?;
        zip.finish()?;
    }
    let source = DocumentSource::from_bytes(cursor.into_inner())?;
    let doc = parse_docx(&source)?;
    assert!(
        doc.warnings
            .iter()
            .any(|warning| warning.code == "DOCX_NUMBERING_FORMAT_MISSING")
    );
    Ok(())
}

#[test]
fn fuzz_table_grid_rejects_invalid_spans() -> Result<(), Box<dyn std::error::Error>> {
    for span_value in ["0", "abc", "-1", "999999999999999999999999"] {
        let doc_xml = format!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"{span_value}\"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"
        );
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("[Content_Types].xml", options)?;
            zip.write_all(b"<Types/>")?;
            zip.start_file("word/document.xml", options)?;
            zip.write_all(doc_xml.as_bytes())?;
            zip.finish()?;
        }
        let source = DocumentSource::from_bytes(cursor.into_inner())?;
        let result = parse_docx(&source);
        assert!(result.is_err(), "span {span_value} must be rejected");
    }
    Ok(())
}

#[test]
fn fuzz_layout_paragraph_never_panics_on_random_text() -> Result<(), Box<dyn std::error::Error>> {
    use docsight_layout::layout_docx;
    let mut state = 42_u64;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (state >> 33) as u8
    };
    for _ in 0..25 {
        let text: String = (0..120)
            .map(|_| {
                let code = 32 + (next() % 95);
                let character = char::from(code);
                if matches!(character, '<' | '>' | '&' | '\'' | '"') {
                    'x'
                } else {
                    character
                }
            })
            .collect();
        let doc_xml = format!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"
        );
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("[Content_Types].xml", options)?;
            zip.write_all(b"<Types/>")?;
            zip.start_file("word/document.xml", options)?;
            zip.write_all(doc_xml.as_bytes())?;
            zip.finish()?;
        }
        let source = DocumentSource::from_bytes(cursor.into_inner())?;
        let parsed = parse_docx(&source)?;
        let laid_out = layout_docx(parsed)?;
        assert!(!laid_out.document.pages.is_empty());
    }
    Ok(())
}

#[test]
fn fuzz_pdf_span_cluster_is_deterministic_and_safe() {
    use docsight_tables::{RulingSegment, TextSpanItem, detect_tables};
    let mut state = 7_u64;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (state >> 33) as u8
    };
    for _ in 0..25 {
        let span_count = (next() as usize % 12) + 1;
        let spans: Vec<TextSpanItem> = (0..span_count)
            .map(|index| TextSpanItem {
                text: format!("cell {index}"),
                bbox: match docsight_core::Rect::new(
                    (next() % 200) as f32,
                    (next() % 200) as f32,
                    ((next() % 200) as f32) + 201.0,
                    ((next() % 200) as f32) + 201.0,
                ) {
                    Ok(rect) => rect,
                    Err(_) => docsight_core::Rect::new(0.0, 0.0, 10.0, 10.0)
                        .unwrap_or_else(|_| unreachable!("static rect is valid")),
                },
                font_size: 10.0,
                bold: false,
            })
            .collect();
        let first = detect_tables(1, &spans, &[], 595.0, 842.0);
        let second = detect_tables(1, &spans, &[], 595.0, 842.0);
        assert_eq!(first.len(), second.len());
        for (left, right) in first.iter().zip(&second) {
            assert_eq!(left.bbox, right.bbox);
            assert_eq!(left.cells.len(), right.cells.len());
        }
    }
    let rulings = vec![
        RulingSegment {
            x0: 10.0,
            y0: 10.0,
            x1: 200.0,
            y1: 10.4,
        },
        RulingSegment {
            x0: 10.0,
            y0: 40.0,
            x1: 200.0,
            y1: 40.4,
        },
        RulingSegment {
            x0: 10.0,
            y0: 10.0,
            x1: 10.4,
            y1: 40.0,
        },
        RulingSegment {
            x0: 200.0,
            y0: 10.0,
            x1: 200.4,
            y1: 40.4,
        },
    ];
    let tables = detect_tables(
        1,
        &[TextSpanItem {
            text: "only".to_owned(),
            bbox: match docsight_core::Rect::new(20.0, 15.0, 60.0, 35.0) {
                Ok(rect) => rect,
                Err(_) => unreachable!("static rect is valid"),
            },
            font_size: 10.0,
            bold: false,
        }],
        &rulings,
        595.0,
        842.0,
    );
    assert!(tables.len() <= 1);
}

#[test]
fn sandbox_memory_limit_kills_worker_as_backend_failure() -> Result<(), Box<dyn std::error::Error>>
{
    let worker_exe = find_worker_binary()?;
    let policy = SandboxPolicy {
        max_memory_bytes: 256 * 1024 * 1024,
        cpu_timeout_secs: 30,
        wall_timeout_secs: 120,
        isolated_temp_dir: false,
        max_output_bytes: 64 * 1024 * 1024,
    };
    let res = docsight_worker::run_in_sandbox_with_env(
        Some(&worker_exe),
        &policy,
        &["--memory-hog-for-test".to_owned(), "inspect".to_owned()],
        &[(
            docsight_worker::SANDBOX_CHILD_ENV.to_owned(),
            "1".to_owned(),
        )],
    );
    match res {
        Err(error) => assert_eq!(error.exit_code(), 30),
        Ok(output) => assert_ne!(
            output.exit_code, 0,
            "the memory hog must not exit successfully under the sandbox limit"
        ),
    }
    Ok(())
}

#[test]
fn sandbox_output_limit_drains_worker_without_deadlock() -> Result<(), Box<dyn std::error::Error>> {
    let worker_exe = find_worker_binary()?;
    let doc_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Projeto_DOCSIGHT_Especificacao.docx");
    let policy = SandboxPolicy {
        max_output_bytes: 1024,
        ..SandboxPolicy::default()
    };
    let start = Instant::now();
    let result = docsight_worker::run_in_sandbox_with_env(
        Some(&worker_exe),
        &policy,
        &["text".to_owned(), doc_path.to_string_lossy().into_owned()],
        &[(
            docsight_worker::SANDBOX_CHILD_ENV.to_owned(),
            "1".to_owned(),
        )],
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(
        matches!(
            &result,
            Err(docsight_core::DocsightError::ResourceLimit { .. })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn sandbox_runs_every_subcommand_through_isolation() -> Result<(), Box<dyn std::error::Error>> {
    let doc_path = fixture("sample_headings.docx");
    let doc_str = doc_path.to_str().ok_or("invalid path")?;
    for subcommand in [
        vec!["tables", doc_str],
        vec!["outline", doc_str, "--json"],
        vec!["text", doc_str, "--json"],
        vec!["query", doc_str, "heading[level<=2]", "--json"],
        vec!["overview", doc_str, "--json"],
        vec![
            "focus",
            doc_str,
            "h_515ad605791c12fc496c1c18d79f6526",
            "--json",
        ],
    ] {
        let output = docsight()
            .args({
                let mut args = subcommand.clone();
                args.push("--sandbox");
                args
            })
            .output()?;
        assert!(
            output.status.success(),
            "sandboxed {:?} failed: {}",
            subcommand,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
