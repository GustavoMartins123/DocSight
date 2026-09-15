use docsight_core::{
    Document, DocumentSource, IR_ENGINE_VERSION, IR_SCHEMA_VERSION, canonical_violations,
};
use docsight_layout::layout_docx;
use docsight_ooxml::parse_docx;
use docsight_pdf::PdfDocument;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const DOCX_FIXTURES: &[&str] = &[
    "sample_features.docx",
    "sample_headings.docx",
    "sample_tables.docx",
];

const PDF_FIXTURES: &[&str] = &[
    "sample_semantic.pdf",
    "sample_table_ruled.pdf",
    "sample_table_alignment.pdf",
    "sample_table_two_ruled.pdf",
    "synthetic_table.pdf",
];

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture(name: &str) -> PathBuf {
    repository_root()
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn ir_golden_path(name: &str) -> PathBuf {
    repository_root()
        .join("fixtures")
        .join("goldens")
        .join("ir")
        .join(format!("{name}.ir.json"))
}

fn ir_schema() -> Result<Value, Box<dyn std::error::Error>> {
    let path = repository_root()
        .join("schemas")
        .join("ir")
        .join("v1")
        .join("document-ir.json");
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn ingest(path: &Path) -> Result<Document, Box<dyn std::error::Error>> {
    let source = DocumentSource::open(path)?;
    match source.format() {
        docsight_core::DocumentFormat::Docx => Ok(layout_docx(parse_docx(&source)?)?.document),
        docsight_core::DocumentFormat::Pdf => Ok(PdfDocument::open(&source)?.to_document()?),
    }
}

fn normalized_snapshot(document: &Document) -> Result<String, Box<dyn std::error::Error>> {
    let mut value = serde_json::to_value(document)?;
    let version = value
        .get_mut("version")
        .and_then(Value::as_object_mut)
        .ok_or("IR snapshot has no version object")?;
    version.insert(
        "engine_version".to_owned(),
        Value::String("<engine>".to_owned()),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn first_difference(expected: &str, current: &str) -> String {
    for (index, (expected_line, current_line)) in expected.lines().zip(current.lines()).enumerate()
    {
        if expected_line != current_line {
            let line = index + 1;
            return format!("line {line}\n  stored:  {expected_line}\n  current: {current_line}");
        }
    }
    format!(
        "line count differs: stored {} lines, current {} lines",
        expected.lines().count(),
        current.lines().count()
    )
}

type BoundCheck = fn(f64, f64) -> bool;

struct SchemaValidator<'a> {
    root: &'a Value,
    errors: Vec<String>,
}

impl<'a> SchemaValidator<'a> {
    fn new(root: &'a Value) -> Self {
        Self {
            root,
            errors: Vec::new(),
        }
    }

    fn resolve(&mut self, schema: &'a Value, pointer: &str) -> Option<&'a Value> {
        let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
            return Some(schema);
        };
        let Some(stripped) = reference.strip_prefix("#/") else {
            self.errors.push(format!(
                "{pointer}: unsupported schema reference {reference}"
            ));
            return None;
        };
        let mut current = self.root;
        for segment in stripped.split('/') {
            match current.get(segment) {
                Some(next) => current = next,
                None => {
                    self.errors.push(format!(
                        "{pointer}: unresolved schema reference {reference}"
                    ));
                    return None;
                }
            }
        }
        Some(current)
    }

    fn validate(&mut self, schema: &'a Value, value: &Value, pointer: &str) {
        let Some(schema) = self.resolve(schema, pointer) else {
            return;
        };
        if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
            let matches = variants
                .iter()
                .filter(|variant| {
                    let mut probe = SchemaValidator::new(self.root);
                    probe.validate(variant, value, pointer);
                    probe.errors.is_empty()
                })
                .count();
            if matches != 1 {
                self.errors.push(format!(
                    "{pointer}: value matched {matches} oneOf variants, expected exactly one"
                ));
            }
            return;
        }
        if let Some(expected) = schema.get("const")
            && expected != value
        {
            self.errors.push(format!(
                "{pointer}: expected constant {expected}, found {value}"
            ));
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
            && !allowed.contains(value)
        {
            self.errors.push(format!(
                "{pointer}: value {value} is not an allowed enum member"
            ));
        }
        if let Some(declared) = schema.get("type") {
            self.check_type(declared, value, pointer);
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
            && let Some(text) = value.as_str()
            && !matches_known_pattern(pattern, text)
        {
            self.errors
                .push(format!("{pointer}: {text:?} does not match {pattern}"));
        }
        self.check_numeric_bounds(schema, value, pointer);
        if let Some(object) = value.as_object() {
            self.check_object(schema, object, pointer);
        }
        if let Some(array) = value.as_array()
            && let Some(items) = schema.get("items")
        {
            for (index, item) in array.iter().enumerate() {
                self.validate(items, item, &format!("{pointer}/{index}"));
            }
        }
    }

    fn check_type(&mut self, declared: &Value, value: &Value, pointer: &str) {
        let names: Vec<&str> = match declared {
            Value::String(name) => vec![name.as_str()],
            Value::Array(entries) => entries.iter().filter_map(Value::as_str).collect(),
            other => {
                self.errors
                    .push(format!("{pointer}: unsupported type declaration {other}"));
                return;
            }
        };
        let matched = names.iter().any(|name| match *name {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        });
        if !matched {
            self.errors.push(format!(
                "{pointer}: expected type {names:?}, found {}",
                type_name(value)
            ));
        }
    }

    fn check_numeric_bounds(&mut self, schema: &'a Value, value: &Value, pointer: &str) {
        let Some(number) = value.as_f64() else {
            return;
        };
        let checks: [(&str, BoundCheck); 3] = [
            ("minimum", |bound, actual| actual >= bound),
            ("maximum", |bound, actual| actual <= bound),
            ("exclusiveMinimum", |bound, actual| actual > bound),
        ];
        for (keyword, satisfied) in checks {
            if let Some(bound) = schema.get(keyword).and_then(Value::as_f64)
                && !satisfied(bound, number)
            {
                self.errors
                    .push(format!("{pointer}: {number} violates {keyword} {bound}"));
            }
        }
    }

    fn check_object(&mut self, schema: &'a Value, object: &Map<String, Value>, pointer: &str) {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    self.errors
                        .push(format!("{pointer}: required field {field} is missing"));
                }
            }
        }
        let closed = schema
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .is_some_and(|allowed| !allowed);
        for (key, child) in object {
            match properties.and_then(|properties| properties.get(key)) {
                Some(child_schema) => {
                    self.validate(child_schema, child, &format!("{pointer}/{key}"));
                }
                None if closed => self.errors.push(format!(
                    "{pointer}: field {key} is not declared by the schema"
                )),
                None => {}
            }
        }
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn matches_known_pattern(pattern: &str, text: &str) -> bool {
    match pattern {
        "^[0-9a-f]{64}$" => {
            text.len() == 64
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }
        "^[A-Z][A-Z0-9_]*$" => {
            let mut bytes = text.bytes();
            bytes.next().is_some_and(|byte| byte.is_ascii_uppercase())
                && bytes
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        }
        _ => false,
    }
}

fn validate_against_schema(
    schema: &Value,
    document: &Document,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = serde_json::to_value(document)?;
    let mut validator = SchemaValidator::new(schema);
    validator.validate(schema, &value, label);
    if !validator.errors.is_empty() {
        return Err(format!(
            "{label} does not satisfy the published IR schema:\n{}",
            validator.errors.join("\n")
        )
        .into());
    }
    Ok(())
}

#[test]
fn ingested_documents_satisfy_the_published_ir_schema() -> Result<(), Box<dyn std::error::Error>> {
    let schema = ir_schema()?;
    for name in DOCX_FIXTURES.iter().chain(PDF_FIXTURES.iter()) {
        let document = ingest(&fixture(name))?;
        validate_against_schema(&schema, &document, name)?;
    }
    let document = ingest(&repository_root().join("Projeto_DOCSIGHT_Especificacao.docx"))?;
    validate_against_schema(&schema, &document, "Projeto_DOCSIGHT_Especificacao.docx")?;
    Ok(())
}

#[test]
fn unknown_pattern_keywords_are_rejected_by_the_contract_test() {
    assert!(matches_known_pattern(
        "^[0-9a-f]{64}$",
        "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
    ));
    assert!(!matches_known_pattern("^[0-9a-f]{64}$", "not-a-digest"));
    assert!(matches_known_pattern(
        "^[A-Z][A-Z0-9_]*$",
        "DOCX_LAYOUT_PAGINATED"
    ));
    assert!(!matches_known_pattern("^[A-Z][A-Z0-9_]*$", "lowercase"));
    assert!(!matches_known_pattern("^unhandled$", "anything"));
}

#[test]
fn ir_declares_the_schema_version_published_in_the_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let schema = ir_schema()?;
    let published = schema
        .get("x-ir-schema-version")
        .and_then(Value::as_str)
        .ok_or("IR schema does not declare x-ir-schema-version")?;
    assert_eq!(published, IR_SCHEMA_VERSION);
    let constrained = schema["$defs"]["irVersion"]["properties"]["schema_version"]["const"]
        .as_str()
        .ok_or("IR schema does not constrain schema_version")?;
    assert_eq!(constrained, IR_SCHEMA_VERSION);

    let document = ingest(&fixture("sample_features.docx"))?;
    assert_eq!(document.version.schema_version, IR_SCHEMA_VERSION);
    assert_eq!(document.version.engine_version, IR_ENGINE_VERSION);
    assert!(document.version.is_current_schema());
    Ok(())
}

#[test]
fn ingestion_is_canonically_ordered_for_every_fixture() -> Result<(), Box<dyn std::error::Error>> {
    for name in DOCX_FIXTURES.iter().chain(PDF_FIXTURES.iter()) {
        let source = DocumentSource::open(fixture(name))?;
        if source.format() == docsight_core::DocumentFormat::Docx {
            let parsed = parse_docx(&source)?;
            let violations = canonical_violations(&parsed);
            assert!(
                violations.is_empty(),
                "{name} violates the canonical contract before layout: {violations:?}"
            );
        }
        let document = ingest(&fixture(name))?;
        let violations = canonical_violations(&document);
        assert!(
            violations.is_empty(),
            "{name} violates the canonical contract after ingestion: {violations:?}"
        );
    }
    Ok(())
}

#[test]
fn repeated_ingestion_produces_byte_identical_ir() -> Result<(), Box<dyn std::error::Error>> {
    for name in DOCX_FIXTURES.iter().chain(PDF_FIXTURES.iter()) {
        let first = serde_json::to_vec(&ingest(&fixture(name))?)?;
        let second = serde_json::to_vec(&ingest(&fixture(name))?)?;
        assert_eq!(first, second, "{name} did not serialize deterministically");
    }
    Ok(())
}

#[test]
fn object_ids_depend_only_on_document_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    for name in DOCX_FIXTURES.iter().chain(PDF_FIXTURES.iter()) {
        let original = ingest(&fixture(name))?;
        let relocated_path = directory.path().join(format!("relocated_{name}"));
        std::fs::copy(fixture(name), &relocated_path)?;
        let relocated = ingest(&relocated_path)?;

        assert_eq!(
            original.id, relocated.id,
            "{name} document id is not stable"
        );
        let original_ids: Vec<String> = original
            .blocks
            .iter()
            .map(|block| block.id.to_string())
            .collect();
        let relocated_ids: Vec<String> = relocated
            .blocks
            .iter()
            .map(|block| block.id.to_string())
            .collect();
        assert_eq!(
            original_ids, relocated_ids,
            "{name} object ids changed with the file path"
        );
    }
    Ok(())
}

#[test]
fn ir_snapshots_are_stable() -> Result<(), Box<dyn std::error::Error>> {
    let update = std::env::var("DOCSIGHT_UPDATE_GOLDENS").is_ok_and(|value| value == "1");
    for name in ["sample_features.docx", "sample_semantic.pdf"] {
        let current = normalized_snapshot(&ingest(&fixture(name))?)?;
        let path = ir_golden_path(name);
        if update {
            std::fs::create_dir_all(path.parent().ok_or("IR golden directory missing")?)?;
            std::fs::write(&path, &current)?;
            continue;
        }
        let stored = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "IR snapshot is missing for {name}; run DOCSIGHT_UPDATE_GOLDENS=1 cargo test to create it: {error}"
            )
        })?;
        let stored = stored.replace("\r\n", "\n");
        assert!(
            stored == current,
            "IR snapshot for {name} changed; review the difference before updating the golden:\n{}",
            first_difference(&stored, &current)
        );
    }
    Ok(())
}
