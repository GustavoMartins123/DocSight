use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const COMPLEXITIES: [&str; 4] = ["simple", "moderate", "complex", "adversarial"];
pub const SIGNAL_METRICS: [&str; 6] = [
    "pages",
    "paragraphs",
    "headings",
    "tables",
    "figures",
    "comments",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentClass {
    pub id: String,
    pub format: String,
    pub complexity: String,
    pub expects_failure: bool,
    pub description: String,
    pub signals: BTreeMap<String, Signal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Taxonomy {
    pub schema: String,
    pub note: String,
    pub classes: Vec<DocumentClass>,
}

impl Taxonomy {
    pub fn get(&self, id: &str) -> Result<&DocumentClass> {
        self.classes
            .iter()
            .find(|class| class.id == id)
            .ok_or_else(|| {
                ToolError::new(
                    "UNKNOWN_DOCUMENT_CLASS",
                    "Corpus cases must use a class from the reviewed taxonomy",
                )
            })
    }

    pub fn ids(&self) -> Vec<&str> {
        self.classes.iter().map(|class| class.id.as_str()).collect()
    }
}

impl DocumentClass {
    /// Reports the signals an inspect result contradicts, which identifies a mislabelled case.
    pub fn violations(&self, measured: &BTreeMap<String, u64>) -> Vec<String> {
        let mut violations = Vec::new();
        for (metric, signal) in &self.signals {
            let Some(value) = measured.get(metric) else {
                violations.push(format!("{metric}:unmeasured"));
                continue;
            };
            if signal.min.is_some_and(|min| *value < min) {
                violations.push(format!("{metric}:below-minimum"));
            }
            if signal.max.is_some_and(|max| *value > max) {
                violations.push(format!("{metric}:above-maximum"));
            }
        }
        violations
    }
}

pub fn validate_taxonomy(taxonomy: Taxonomy) -> Result<Taxonomy> {
    require(
        taxonomy.schema == "docsight.document-classes/v1"
            && (1..=200).contains(&taxonomy.classes.len())
            && !taxonomy.note.is_empty(),
        "INVALID_DOCUMENT_CLASSES",
        "A bounded versioned document class taxonomy is required",
    )?;
    let mut seen = BTreeSet::new();
    for class in &taxonomy.classes {
        let valid_id = (1..=60).contains(&class.id.len())
            && class
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !class.id.starts_with('-');
        require(
            valid_id && seen.insert(class.id.clone()),
            "INVALID_DOCUMENT_CLASSES",
            "Class identifiers must be unique portable slugs",
        )?;
        require(
            crate::corpus::FORMATS.contains(&class.format.as_str())
                && COMPLEXITIES.contains(&class.complexity.as_str())
                && (1..=400).contains(&class.description.len()),
            "INVALID_DOCUMENT_CLASSES",
            "Class format, complexity or description is unsupported",
        )?;
        require(
            class.expects_failure == (class.complexity == "adversarial"),
            "INVALID_DOCUMENT_CLASSES",
            "Only adversarial classes expect the engine to reject the document",
        )?;
        require(
            class.expects_failure == class.signals.is_empty(),
            "INVALID_DOCUMENT_CLASSES",
            "A rejected class has no observable signals and a document class needs at least one",
        )?;
        for (metric, signal) in &class.signals {
            require(
                SIGNAL_METRICS.contains(&metric.as_str()),
                "INVALID_DOCUMENT_CLASSES",
                "Class signals must use published inspect metrics",
            )?;
            require(
                (signal.min.is_some() || signal.max.is_some())
                    && signal.min.unwrap_or(0) <= signal.max.unwrap_or(u64::MAX)
                    && signal.min.unwrap_or(0) <= 100_000
                    && signal.max.unwrap_or(0) <= 100_000,
                "INVALID_DOCUMENT_CLASSES",
                "Class signals must be bounded and consistent",
            )?;
        }
    }
    Ok(taxonomy)
}

pub fn load_taxonomy(root: &Path) -> Result<Taxonomy> {
    validate_taxonomy(decode(read_json(
        &root.join("release/document-classes.json"),
    )?)?)
}

/// Reads the taxonomy from an explicit path, or from the repository that owns this tool, so a
/// corpus kept outside the repository is still classified by the reviewed taxonomy.
pub fn read_taxonomy(path: Option<&Path>) -> Result<Taxonomy> {
    match path {
        Some(path) => validate_taxonomy(decode(read_json(path)?)?),
        None => load_taxonomy(&workspace_root()),
    }
}
