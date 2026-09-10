use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("fixtures").join("validation").join(name)
}

fn run(args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(docsight().args(args).output()?)
}

fn success_json(args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let output = run(args)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn warning_codes(value: &Value) -> Value {
    Value::Array(
        value["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|warning| warning["code"].clone())
            .collect(),
    )
}

fn error_record(name: &str, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
    let output = run(args)?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr)?;
    Ok(json!({
        "name": name,
        "exit_code": output.status.code(),
        "diagnostic": value["error"]["code"],
        "effect": value["error"]["effect"]
    }))
}

fn rebase_local_refs(node: &Value, base: &str) -> Value {
    match node {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                match (key.as_str(), value.as_str()) {
                    ("$ref", Some(target)) if target.starts_with('#') => {
                        out.insert(key.clone(), Value::from(format!("{base}{}", &target[1..])));
                    }
                    _ => {
                        out.insert(key.clone(), rebase_local_refs(value, base));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| rebase_local_refs(item, base))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn inlined_schema_definitions_match_their_published_files() -> Result<(), Box<dyn std::error::Error>>
{
    let schema_root = workspace().join("schemas").join("v2");
    let mut paths = std::fs::read_dir(&schema_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();

    let mut compared = 0_usize;
    for path in &paths {
        let schema: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let Some(defs) = schema["$defs"].as_object() else {
            continue;
        };
        let file = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("schema filename")?;
        for (name, inlined) in defs {
            let published = schema_root.join(format!("{}.json", name.replace('_', "-")));
            if !published.exists() {
                continue;
            }
            let mut expected: Value = serde_json::from_slice(&std::fs::read(&published)?)?;
            if let Some(map) = expected.as_object_mut() {
                map.remove("$schema");
                map.remove("$id");
                map.remove("title");
            }
            let expected = rebase_local_refs(&expected, &format!("#/$defs/{name}"));
            assert_eq!(
                inlined,
                &expected,
                "{file} inlines a stale copy of {}.json",
                name.replace('_', "-")
            );
            compared += 1;
        }
    }
    assert!(
        compared > 0,
        "no inlined schema definition was compared against its published file"
    );
    Ok(())
}

fn collect_non_local_refs(node: &Value, found: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value.as_str()) {
                    ("$ref", Some(target)) if !target.starts_with('#') => {
                        found.push(target.to_owned());
                    }
                    _ => collect_non_local_refs(value, found),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_non_local_refs(item, found);
            }
        }
        _ => {}
    }
}

#[test]
fn published_schemas_are_self_contained() -> Result<(), Box<dyn std::error::Error>> {
    let schema_root = workspace().join("schemas").join("v2");
    let mut paths = std::fs::read_dir(schema_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    assert!(!paths.is_empty(), "no published schemas were found");

    let mut offenders = Vec::new();
    for path in &paths {
        let schema: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut found = Vec::new();
        collect_non_local_refs(&schema, &mut found);
        let file = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("schema filename")?;
        for target in found {
            offenders.push(format!("{file} -> {target}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "schemas must validate offline, so every $ref has to stay inside its own file: {}",
        offenders.join(", ")
    );
    Ok(())
}

fn schema_records() -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let schema_root = workspace().join("schemas").join("v2");
    let mut paths = std::fs::read_dir(schema_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path)?;
            let schema: Value = serde_json::from_slice(&bytes)?;
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("schema filename")?;
            let digest = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            Ok(json!({
                "file": file,
                "id": schema["$id"],
                "sha256": digest
            }))
        })
        .collect()
}

fn render_record(
    input: &Path,
    page: &str,
    dpi: &str,
    output: &Path,
    trace: &Path,
) -> Result<Value, Box<dyn std::error::Error>> {
    let input = input.to_str().ok_or("input path")?;
    let output = output.to_str().ok_or("output path")?;
    let trace = trace.to_str().ok_or("trace path")?;
    let value = success_json(&[
        "--agent", "render", input, "--page", page, "--dpi", dpi, "--out", output, "--trace", trace,
    ])?;
    Ok(json!({
        "document": value["document"],
        "page": value["result"]["page"],
        "bbox": value["result"]["bbox"],
        "dpi": value["result"]["dpi"],
        "width_px": value["result"]["width_px"],
        "height_px": value["result"]["height_px"],
        "raster_sha256": value["result"]["output_sha256"],
        "display_list_sha256": value["result"]["trace"]["display_list_sha256"],
        "trace_sha256": value["result"]["trace"]["output_sha256"],
        "decision_coverage": value["result"]["trace"]["decision_coverage"],
        "resource_count": value["result"]["trace"]["resource_count"],
        "glyph_run_count": value["result"]["trace"]["glyph_run_count"],
        "table_sizing_decision_count": value["result"]["trace"]["table_sizing_decision_count"],
        "pagination_decision_count": value["result"]["trace"]["pagination_decision_count"],
        "display_operation_count": value["result"]["trace"]["display_operation_count"],
        "warnings": warning_codes(&value)
    }))
}

fn replay_record(trace: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let trace = trace.to_str().ok_or("trace path")?;
    let value = success_json(&["--agent", "replay", trace, "--verify"])?;
    Ok(json!({
        "trace_schema": value["result"]["trace_schema"],
        "target": value["result"]["target"],
        "decision_coverage": value["result"]["decision_coverage"],
        "verification": value["result"]["verification"],
        "warnings": warning_codes(&value)
    }))
}

#[test]
fn evidence_engine_matches_cross_platform_conformance_golden()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let docx = fixture("sample_headings.docx");
    let pdf = fixture("synthetic_table.pdf");
    let docx_str = docx.to_str().ok_or("DOCX path")?;
    let pdf_str = pdf.to_str().ok_or("PDF path")?;
    let docx_object = "h_515ad605791c12fc496c1c18d79f6526";
    let pdf_object = "tbl_b54dbb45da2a78dec493d24c309ea8c0";

    let evidence_docx = success_json(&[
        "--agent",
        "evidence",
        docx_str,
        docx_object,
        "--render-dpi",
        "72",
    ])?;
    let evidence_docx_repeat = success_json(&[
        "--agent",
        "evidence",
        docx_str,
        docx_object,
        "--render-dpi",
        "72",
    ])?;
    assert_eq!(evidence_docx, evidence_docx_repeat);

    let coverage_docx = success_json(&["--agent", "coverage", docx_str, "--regions"])?;
    let coverage_docx_repeat = success_json(&["--agent", "coverage", docx_str, "--regions"])?;
    assert_eq!(coverage_docx, coverage_docx_repeat);

    let hit_docx = success_json(&[
        "--agent", "hit", docx_str, "--page", "1", "--point", "100,80",
    ])?;
    let lineage_docx = success_json(&["--agent", "diff", docx_str, docx_str])?;
    let evidence_pdf = success_json(&[
        "--agent",
        "evidence",
        pdf_str,
        pdf_object,
        "--render-dpi",
        "36",
    ])?;
    let coverage_pdf = success_json(&["--agent", "coverage", pdf_str, "--regions"])?;

    let docx_render = render_record(
        &docx,
        "1",
        "72",
        &temporary.path().join("docx.png"),
        &temporary.path().join("docx.dstrace"),
    )?;
    let docx_render_repeat = render_record(
        &docx,
        "1",
        "72",
        &temporary.path().join("docx-repeat.png"),
        &temporary.path().join("docx-repeat.dstrace"),
    )?;
    assert_eq!(docx_render, docx_render_repeat);
    assert_eq!(
        std::fs::read(temporary.path().join("docx.png"))?,
        std::fs::read(temporary.path().join("docx-repeat.png"))?
    );
    assert_eq!(
        std::fs::read(temporary.path().join("docx.dstrace"))?,
        std::fs::read(temporary.path().join("docx-repeat.dstrace"))?
    );

    let pdf_render = render_record(
        &pdf,
        "1",
        "36",
        &temporary.path().join("pdf.png"),
        &temporary.path().join("pdf.dstrace"),
    )?;
    let pdf_render_repeat = render_record(
        &pdf,
        "1",
        "36",
        &temporary.path().join("pdf-repeat.png"),
        &temporary.path().join("pdf-repeat.dstrace"),
    )?;
    assert_eq!(pdf_render, pdf_render_repeat);
    assert_eq!(
        std::fs::read(temporary.path().join("pdf.png"))?,
        std::fs::read(temporary.path().join("pdf-repeat.png"))?
    );
    assert_eq!(
        std::fs::read(temporary.path().join("pdf.dstrace"))?,
        std::fs::read(temporary.path().join("pdf-repeat.dstrace"))?
    );

    let unsupported = temporary.path().join("unsupported.bin");
    std::fs::write(&unsupported, b"not a document")?;
    let malformed = temporary.path().join("malformed.pdf");
    std::fs::write(&malformed, b"%PDF-1.7\ninvalid\n%%EOF\n")?;
    let unsupported_str = unsupported.to_str().ok_or("unsupported path")?;
    let malformed_str = malformed.to_str().ok_or("malformed path")?;

    let actual = json!({
        "schema": "docsight.conformance/v1",
        "public_schemas": schema_records()?,
        "docx": {
            "evidence": {
                "document": evidence_docx["document"],
                "result": evidence_docx["result"],
                "warnings": warning_codes(&evidence_docx)
            },
            "coverage": coverage_docx["result"],
            "hit": hit_docx["result"],
            "lineage": {
                "summary": lineage_docx["result"]["summary"],
                "total_records": lineage_docx["result"]["semantic"]["lineage"].as_array().map(Vec::len),
                "first_record": lineage_docx["result"]["semantic"]["lineage"][0]
            },
            "render": docx_render,
            "replay": replay_record(&temporary.path().join("docx.dstrace"))?
        },
        "pdf": {
            "evidence": {
                "document": evidence_pdf["document"],
                "result": evidence_pdf["result"],
                "warnings": warning_codes(&evidence_pdf)
            },
            "coverage": coverage_pdf["result"],
            "render": pdf_render,
            "replay": replay_record(&temporary.path().join("pdf.dstrace"))?
        },
        "failures": [
            error_record("unsupported_format", &["--agent", "inspect", unsupported_str])?,
            error_record("malformed_document", &["--agent", "inspect", malformed_str])?,
            error_record("object_not_found", &["--agent", "evidence", docx_str, "missing_object"] )?,
            error_record("output_limit", &["--agent", "--max-bytes", "64", "inspect", docx_str])?
        ]
    });

    let golden_path = workspace()
        .join("fixtures")
        .join("conformance")
        .join("m15-evidence-engine.json");
    if std::env::var("DOCSIGHT_UPDATE_GOLDENS").is_ok_and(|value| value == "1") {
        let mut serialized = serde_json::to_string_pretty(&actual)?;
        serialized.push('\n');
        std::fs::write(&golden_path, serialized)?;
        return Ok(());
    }
    let expected: Value = serde_json::from_slice(&std::fs::read(golden_path)?)?;
    assert_eq!(actual, expected);
    Ok(())
}
