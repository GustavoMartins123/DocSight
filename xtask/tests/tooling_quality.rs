#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::BTreeMap;
use support::*;
use xtask::corpus::load_manifest;
use xtask::quality::{Taxonomy, validate_taxonomy};
use xtask::tooling::common::workspace_root;

fn class_document(overrides: serde_json::Value) -> TestResult<Taxonomy> {
    let mut class = json!({
        "id": "docx-sample",
        "format": "docx",
        "complexity": "simple",
        "expects_failure": false,
        "description": "Sample class.",
        "signals": {"paragraphs": {"min": 1}},
    });
    if let (Some(target), Some(source)) = (class.as_object_mut(), overrides.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(serde_json::from_value(json!({
        "schema": "docsight.document-classes/v1",
        "note": "Test taxonomy.",
        "classes": [class],
    }))?)
}

#[test]
fn repository_taxonomy_classifies_every_corpus_case() -> TestResult {
    let root = workspace_root();
    let taxonomy = taxonomy();
    assert!(taxonomy.classes.len() >= 10);
    for format in ["docx", "pdf", "invalid"] {
        assert!(
            taxonomy.classes.iter().any(|class| class.format == format),
            "no class describes {format}"
        );
    }
    let manifest = load_manifest(&root.join("release/corpus.json"), Some(&root), &taxonomy)?;
    for case in &manifest.cases {
        let class = taxonomy.get(&case.class)?;
        assert_eq!(class.format, case.format, "{}", case.id);
        assert_eq!(
            class.expects_failure,
            case.expected.exit_code != 0,
            "{}",
            case.id
        );
    }
    Ok(())
}

#[test]
fn invalid_taxonomies_fail_with_a_specific_code() -> TestResult {
    let mut variants: Vec<Taxonomy> = Vec::new();

    let mut taxonomy = class_document(json!({}))?;
    taxonomy.schema = "docsight.document-classes/v2".into();
    variants.push(taxonomy);

    let mut taxonomy = class_document(json!({}))?;
    taxonomy.classes.push(taxonomy.classes[0].clone());
    variants.push(taxonomy);

    variants.push(class_document(json!({"id": "Upper"}))?);
    variants.push(class_document(json!({"format": "xlsx"}))?);
    variants.push(class_document(json!({"complexity": "extreme"}))?);
    variants.push(class_document(json!({"expects_failure": true}))?);
    variants.push(class_document(json!({"signals": {}}))?);
    variants.push(class_document(
        json!({"complexity": "adversarial", "expects_failure": true}),
    )?);
    variants.push(class_document(json!({"signals": {"words": {"min": 1}}}))?);
    variants.push(class_document(json!({"signals": {"pages": {}}}))?);
    variants.push(class_document(
        json!({"signals": {"pages": {"min": 3, "max": 2}}}),
    )?);
    variants.push(class_document(json!({"description": ""}))?);

    for taxonomy in variants {
        assert_eq!(
            code(validate_taxonomy(taxonomy)),
            Some("INVALID_DOCUMENT_CLASSES")
        );
    }
    assert!(validate_taxonomy(class_document(json!({}))?).is_ok());
    assert!(
        validate_taxonomy(class_document(json!({
            "format": "invalid",
            "complexity": "adversarial",
            "expects_failure": true,
            "signals": {},
        }))?)
        .is_ok()
    );
    Ok(())
}

#[test]
fn class_signals_expose_a_mislabelled_document() -> TestResult {
    let taxonomy = taxonomy();
    let tabular = taxonomy.get("docx-tabular")?;
    let text = taxonomy.get("docx-text")?;
    let measured = BTreeMap::from([
        ("pages".to_owned(), 2),
        ("paragraphs".to_owned(), 30),
        ("headings".to_owned(), 0),
        ("tables".to_owned(), 3),
        ("figures".to_owned(), 0),
    ]);
    assert!(tabular.violations(&measured).is_empty());
    assert_eq!(text.violations(&measured), vec!["tables:above-maximum"]);

    let no_tables = BTreeMap::from([("tables".to_owned(), 0)]);
    assert_eq!(tabular.violations(&no_tables), vec!["tables:below-minimum"]);
    assert_eq!(
        tabular.violations(&BTreeMap::new()),
        vec!["tables:unmeasured"]
    );
    Ok(())
}
