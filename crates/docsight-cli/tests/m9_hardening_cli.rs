use docsight_core::DocumentSource;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use docsight_worker::{SandboxPolicy, find_worker_binary, run_in_sandbox};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

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
        .args(["inspect", doc_str, "--sandbox"])
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Format:     docx"));
    assert!(stdout.contains("Pages:      3"));

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
    assert_eq!(val["schema"], "docsight.agent/v1");
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
