#[allow(dead_code)]
mod support;

use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::*;
use xtask::architecture::audit;
use xtask::beta::{CollectOptions, collect_with};
use xtask::corpus::{load_manifest, run_corpus_with};
use xtask::readiness::assess;
use xtask::release::archive::verify_archive;
use xtask::release::{DOCUMENTS, native_target};
use xtask::smoke::smoke_archive_with;
use xtask::tooling::common::{ToolError, read_bytes, read_json, text, workspace_root};
use xtask::tooling::process::ProcessLimits;
use xtask::validation::run_with;

const ANNOTATIONS: [&str; 5] = ["$schema", "$id", "$defs", "title", "description"];
const KEYWORDS: [&str; 20] = [
    "$ref",
    "oneOf",
    "anyOf",
    "const",
    "enum",
    "type",
    "pattern",
    "minimum",
    "maximum",
    "minLength",
    "minItems",
    "maxItems",
    "uniqueItems",
    "items",
    "required",
    "properties",
    "additionalProperties",
    "maxProperties",
    "propertyNames",
    "default",
];

struct Validator<'a> {
    root: &'a Value,
    errors: Vec<String>,
}

impl<'a> Validator<'a> {
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
        let target = reference
            .strip_prefix('#')
            .and_then(|fragment| self.root.pointer(fragment));
        if target.is_none() {
            self.errors
                .push(format!("{pointer}: unresolved reference {reference}"));
        }
        target
    }

    fn matches(&self, schema: &'a Value, value: &Value, pointer: &str) -> bool {
        let mut probe = Validator::new(self.root);
        probe.validate(schema, value, pointer);
        probe.errors.is_empty()
    }

    fn validate(&mut self, schema: &'a Value, value: &Value, pointer: &str) {
        let Some(schema) = self.resolve(schema, pointer) else {
            return;
        };
        if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
            let count = variants
                .iter()
                .filter(|variant| self.matches(variant, value, pointer))
                .count();
            if count != 1 {
                self.errors
                    .push(format!("{pointer}: matched {count} oneOf variants"));
            }
        }
        if let Some(variants) = schema.get("anyOf").and_then(Value::as_array)
            && !variants
                .iter()
                .any(|variant| self.matches(variant, value, pointer))
        {
            self.errors
                .push(format!("{pointer}: matched no anyOf variant"));
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
            self.errors
                .push(format!("{pointer}: {value} is not allowed"));
        }
        if let Some(declared) = schema.get("type") {
            self.check_type(declared, value, pointer);
        }
        self.check_string(schema, value, pointer);
        self.check_number(schema, value, pointer);
        if let Some(items) = value.as_array() {
            self.check_array(schema, items, pointer);
        }
        if let Some(object) = value.as_object() {
            self.check_object(schema, object, pointer);
        }
    }

    fn check_type(&mut self, declared: &Value, value: &Value, pointer: &str) {
        let names: Vec<&str> = match declared {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
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
            self.errors
                .push(format!("{pointer}: {value} is not of type {names:?}"));
        }
    }

    fn check_string(&mut self, schema: &Value, value: &Value, pointer: &str) {
        let Some(text) = value.as_str() else {
            return;
        };
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
            match Regex::new(pattern) {
                Ok(expression) if expression.is_match(text) => {}
                Ok(_) => self
                    .errors
                    .push(format!("{pointer}: {text:?} does not match {pattern}")),
                Err(_) => self
                    .errors
                    .push(format!("{pointer}: invalid pattern {pattern}")),
            }
        }
        if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
            && (text.chars().count() as u64) < minimum
        {
            self.errors.push(format!("{pointer}: string is too short"));
        }
    }

    fn check_number(&mut self, schema: &Value, value: &Value, pointer: &str) {
        if !value.is_number() {
            return;
        }
        let as_i128 = |number: &Value| {
            number
                .as_i64()
                .map(i128::from)
                .or_else(|| number.as_u64().map(i128::from))
        };
        let (Some(actual), minimum, maximum) = (
            as_i128(value),
            schema.get("minimum").and_then(as_i128),
            schema.get("maximum").and_then(as_i128),
        ) else {
            return;
        };
        if minimum.is_some_and(|bound| actual < bound)
            || maximum.is_some_and(|bound| actual > bound)
        {
            self.errors
                .push(format!("{pointer}: {actual} is outside its bounds"));
        }
    }

    fn check_array(&mut self, schema: &'a Value, items: &[Value], pointer: &str) {
        let length = items.len() as u64;
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
            || schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|maximum| length > maximum)
        {
            self.errors.push(format!(
                "{pointer}: array length {length} is outside its bounds"
            ));
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            let distinct: BTreeSet<String> = items.iter().map(Value::to_string).collect();
            if distinct.len() != items.len() {
                self.errors
                    .push(format!("{pointer}: array items are not unique"));
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                self.validate(item_schema, item, &format!("{pointer}/{index}"));
            }
        }
    }

    fn check_object(&mut self, schema: &'a Value, object: &Map<String, Value>, pointer: &str) {
        for field in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(field) {
                self.errors
                    .push(format!("{pointer}: required field {field} is missing"));
            }
        }
        if schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| object.len() as u64 > maximum)
        {
            self.errors.push(format!("{pointer}: too many properties"));
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            if let Some(names) = schema.get("propertyNames") {
                self.validate(
                    names,
                    &Value::String(key.clone()),
                    &format!("{pointer}/{key}"),
                );
            }
            match (
                properties.and_then(|properties| properties.get(key)),
                schema.get("additionalProperties"),
            ) {
                (Some(child_schema), _) => {
                    self.validate(child_schema, child, &format!("{pointer}/{key}"))
                }
                (None, Some(Value::Bool(false))) => self
                    .errors
                    .push(format!("{pointer}: undeclared field {key}")),
                (None, Some(additional)) if additional.is_object() => {
                    self.validate(additional, child, &format!("{pointer}/{key}"))
                }
                _ => {}
            }
        }
    }
}

fn evidence_schema() -> TestResult<Value> {
    Ok(read_json(
        &workspace_root().join("schemas/tooling/v2/evidence.json"),
    )?)
}

fn assert_contract(schema: &Value, definition: &str, value: &Value) -> TestResult {
    let reference = json!({"$ref": format!("#/$defs/{definition}")});
    let mut validator = Validator::new(schema);
    validator.validate(&reference, value, definition);
    if !validator.errors.is_empty() {
        return Err(format!("{definition}:\n{}", validator.errors.join("\n")).into());
    }
    let variants = schema["oneOf"].as_array().ok_or("oneOf")?;
    let matching: Vec<_> = variants
        .iter()
        .filter(|variant| Validator::new(schema).matches(variant, value, "root"))
        .collect();
    if matching.len() != 1 {
        return Err(format!("{definition} matched {} root variants", matching.len()).into());
    }
    Ok(())
}

fn collect_keywords(value: &Value, path: &str, unknown: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, child) in object {
        match key.as_str() {
            "properties" | "$defs" => {
                for (name, nested) in child.as_object().into_iter().flatten() {
                    collect_keywords(nested, &format!("{path}/{key}/{name}"), unknown);
                }
            }
            "oneOf" | "anyOf" => {
                for (index, nested) in child.as_array().into_iter().flatten().enumerate() {
                    collect_keywords(nested, &format!("{path}/{key}/{index}"), unknown);
                }
            }
            "items" | "additionalProperties" | "propertyNames" => {
                collect_keywords(child, &format!("{path}/{key}"), unknown)
            }
            keyword if KEYWORDS.contains(&keyword) || ANNOTATIONS.contains(&keyword) => {}
            keyword => unknown.push(format!("{path}/{keyword}")),
        }
    }
}

#[test]
fn evidence_schema_uses_only_validated_keywords_and_resolvable_references() -> TestResult {
    let schema = evidence_schema()?;
    let mut unknown = Vec::new();
    collect_keywords(&schema, "#", &mut unknown);
    assert!(unknown.is_empty(), "{unknown:?}");
    let text = fs::read_to_string(workspace_root().join("schemas/tooling/v2/evidence.json"))?;
    for reference in Regex::new(r##""\$ref": "#([^"]+)""##)?.captures_iter(&text) {
        assert!(schema.pointer(&reference[1]).is_some(), "{}", &reference[1]);
    }
    for pattern in Regex::new(r#""pattern": "((?:[^"\\]|\\.)*)""#)?.captures_iter(&text) {
        let decoded: String = serde_json::from_str(&format!("\"{}\"", &pattern[1]))?;
        Regex::new(&decoded)?;
    }
    Ok(())
}

#[test]
fn contract_validator_rejects_undeclared_fields_and_wrong_constants() -> TestResult {
    let schema = evidence_schema()?;
    let error = serde_json::to_value(ToolError::new("IO_ERROR", "failure"))?;
    assert_contract(&schema, "tooling-error", &error)?;
    let mut extra = error.clone();
    extra["path"] = json!("/private/location");
    assert!(assert_contract(&schema, "tooling-error", &extra).is_err());
    let mut relabeled = error;
    relabeled["schema"] = json!("docsight.tooling-error/v2");
    assert!(assert_contract(&schema, "tooling-error", &relabeled).is_err());
    Ok(())
}

#[test]
fn repository_corpus_manifest_satisfies_its_published_contract() -> TestResult {
    let schema = evidence_schema()?;
    let root = workspace_root();
    let path = root.join("release/corpus.json");
    load_manifest(&path, Some(&root), &taxonomy())?;
    assert_contract(&schema, "corpus-manifest", &read_json(&path)?)
}

#[test]
fn generated_release_and_smoke_artifacts_satisfy_their_contracts() -> TestResult {
    let schema = evidence_schema()?;
    let fixture = Fixture::new()?;
    let archive = fixture.package(native_target()?, "dist")?;
    assert_contract(
        &schema,
        "release-manifest",
        &serde_json::to_value(verify_archive(&archive)?)?,
    )?;
    let mut runner = Callback(synthetic_engine);
    let passing = smoke_archive_with(&archive, &mut runner)?;
    assert_contract(&schema, "smoke-report", &serde_json::to_value(&passing)?)?;
    let mut failing = Callback(
        |arguments: &[OsString],
         cwd: &Path,
         limits: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>| {
            if arguments.iter().any(|argument| argument == "capabilities") {
                return Err(ToolError::new("EXECUTABLE_UNAVAILABLE", "unavailable"));
            }
            synthetic_engine(arguments, cwd, limits, environment)
        },
    );
    let receipt = smoke_archive_with(&archive, &mut failing)?;
    assert!(!receipt.passed);
    assert_contract(&schema, "smoke-report", &serde_json::to_value(&receipt)?)
}

#[test]
fn generated_corpus_and_beta_reports_satisfy_their_contracts() -> TestResult {
    let schema = evidence_schema()?;
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join("doc.docx"), b"document")?;
    let archive = fixture.package(native_target()?, "dist")?;
    let manifest = fixture.root.join("corpus.json");
    save(&manifest, &one_case("doc.docx", b"document", "inspect"))?;
    let mut runner = Callback(synthetic_engine);
    let report = run_corpus_with(&archive, &manifest, &fixture.root, &taxonomy(), &mut runner)?;
    assert!(report.passed);
    assert_contract(&schema, "corpus-report", &serde_json::to_value(&report)?)?;

    let options = CollectOptions {
        participant: "beta-007".into(),
        operation: "inspect".into(),
        experience: "confusing".into(),
        consent: true,
        document: Some(fixture.root.join("doc.docx")),
        reference: None,
        password_file: None,
        include_document_digest: true,
    };
    let observation = collect_with(&archive, &options, &mut runner)?;
    assert_contract(&schema, "beta-report", &serde_json::to_value(&observation)?)
}

#[test]
fn generated_validation_readiness_and_architecture_reports_satisfy_their_contracts() -> TestResult {
    let schema = evidence_schema()?;
    let fixture = Fixture::new()?;
    let mut runner = Callback(
        |arguments: &[OsString],
         _: &Path,
         _: &ProcessLimits,
         _: Option<&BTreeMap<OsString, OsString>>| {
            let program = arguments
                .first()
                .map(|value| value.to_string_lossy().into_owned());
            if arguments.iter().any(|argument| argument == "rev-parse") {
                return Ok(outcome(0, format!("{REVISION}\n").into_bytes(), Vec::new()));
            }
            if arguments.iter().any(|argument| argument == "status") {
                return Ok(outcome(0, Vec::new(), Vec::new()));
            }
            if program.as_deref() == Some("cargo") {
                return Err(ToolError::new("EXECUTABLE_UNAVAILABLE", "unavailable"));
            }
            Ok(outcome(0, Vec::new(), Vec::new()))
        },
    );
    let validation = run_with(
        &fixture.root.join("validation"),
        &fixture.root,
        &fixture.root.join("maint"),
        &mut runner,
    )?;
    assert!(!validation.passed);
    assert_contract(
        &schema,
        "validation-report",
        &serde_json::to_value(&validation)?,
    )?;

    let readiness = assess(&fixture.root.join("absent"), REVISION, &workspace_root())?;
    assert_contract(
        &schema,
        "readiness-report",
        &serde_json::to_value(&readiness)?,
    )?;

    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.root)
        .status()?;
    assert!(status.success());
    assert_contract(&schema, "architecture-check", &audit(&fixture.root)?)
}

fn operational_files() -> TestResult<Vec<(String, String)>> {
    let root = workspace_root();
    let mut files = Vec::new();
    for entry in fs::read_dir(root.join(".github/workflows"))? {
        let path = entry?.path();
        let name = format!(
            ".github/workflows/{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or("name")?
        );
        files.push((name, text(&read_bytes(&path, 1_048_576)?)?.to_owned()));
    }
    for name in DOCUMENTS.into_iter().filter(|name| name.ends_with(".md")) {
        files.push((
            name.to_owned(),
            text(&read_bytes(&root.join(name), 1_048_576)?)?.to_owned(),
        ));
    }
    files.sort();
    Ok(files)
}

#[test]
fn documented_and_ci_xtask_runs_select_an_explicit_binary() -> TestResult {
    let mut violations = Vec::new();
    for (name, content) in operational_files()? {
        for (index, line) in content.lines().enumerate() {
            if line.contains("cargo run") && line.contains("-p xtask") && !line.contains("--bin ") {
                violations.push(format!("{name}:{}", index + 1));
            }
        }
    }
    assert!(violations.is_empty(), "{violations:?}");
    let usage = fs::read_to_string(workspace_root().join("xtask/src/main.rs"))?;
    assert!(usage.contains("cargo run --locked --release -p xtask --bin xtask -- benchmark"));
    Ok(())
}

#[test]
fn notices_metadata_is_filtered_to_the_packaged_target() -> TestResult {
    let mut invocations = 0;
    for (name, content) in operational_files()? {
        for (index, line) in content.lines().enumerate() {
            if line.contains("cargo metadata") {
                invocations += 1;
                assert!(
                    line.contains("--locked") && line.contains("--filter-platform \"$TARGET\""),
                    "{name}:{}",
                    index + 1
                );
            }
        }
    }
    assert!(invocations >= 2);
    Ok(())
}

#[test]
fn workflows_pin_the_repository_toolchain_and_lock_dependencies() -> TestResult {
    let toolchain = fs::read_to_string(workspace_root().join("rust-toolchain.toml"))?;
    let channel = Regex::new(r#"channel = "([^"]+)""#)?
        .captures(&toolchain)
        .map(|captures| captures[1].to_owned())
        .ok_or("channel")?;
    let pinned = Regex::new(r#"toolchain: "([^"]+)""#)?;
    let cargo = Regex::new(r"\bcargo (build|test|clippy|run|metadata)\b")?;
    let mut violations = Vec::new();
    for (name, content) in operational_files()?
        .into_iter()
        .filter(|(name, _)| name.starts_with(".github/"))
    {
        for (index, line) in content.lines().enumerate() {
            if let Some(captures) = pinned.captures(line)
                && captures[1] != channel
            {
                violations.push(format!("{name}:{} pins {}", index + 1, &captures[1]));
            }
            if cargo.is_match(line) && !line.contains("--locked") {
                violations.push(format!("{name}:{} is not locked", index + 1));
            }
        }
    }
    assert!(violations.is_empty(), "{violations:?}");
    Ok(())
}
