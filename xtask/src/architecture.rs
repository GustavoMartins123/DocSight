use crate::tooling::{
    common::*,
    process::{ProcessLimits, run_bounded},
};
use serde_json::{Value, json};
use std::path::Path;

pub fn audit(root: &Path) -> Result<Value> {
    let result = run_bounded(
        &[
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        root,
        &ProcessLimits {
            output_bytes: 4_194_304,
            ..ProcessLimits::default()
        },
        None,
    )?;
    require(
        result.returncode == 0 && result.termination.is_none(),
        "GIT_STATE_UNAVAILABLE",
        "Cannot enumerate project files",
    )?;
    let mut violations = Vec::new();
    let mut count = 0;
    for raw in result
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = text(raw)?;
        safe_member(name)?;
        count += 1;
        let lower_name = name.to_ascii_lowercase();
        let foreign_source = [".py", ".pyc", ".pyw", ".js", ".ts", ".go"]
            .iter()
            .any(|extension| lower_name.ends_with(extension));
        let is_text_file = [
            ".rs",
            ".json",
            ".toml",
            ".md",
            ".yml",
            ".yaml",
            ".lock",
            ".gitignore",
            ".gitattributes",
        ]
        .iter()
        .any(|extension| lower_name.ends_with(extension))
            || lower_name.starts_with("license");
        if is_text_file {
            let bytes = read_bytes(&contained_file(root, name)?, MAX_FILE_BYTES)?;
            if bytes.contains(&b'\r') {
                violations.push(format!("{name} (contains CRLF line endings)"));
            }
        }
        if foreign_source || lower_name.split('/').any(|part| part == "__pycache__") {
            violations.push(name.to_owned());
        } else if name.starts_with(".github/workflows/")
            || name.ends_with("Cargo.toml")
            || lower_name.ends_with(".md")
        {
            let bytes = read_bytes(&contained_file(root, name)?, MAX_JSON_BYTES)?;
            let content = text(&bytes)?.to_ascii_lowercase();
            let configuration = !lower_name.ends_with(".md");
            let runtime = configuration
                && ["python", "pip ", "pip3 ", "pyo3"]
                    .iter()
                    .any(|needle| content.contains(needle));
            let command = [
                "python -",
                "python3 -",
                "python scripts/",
                "python3 scripts/",
                "pip install",
                "pip3 install",
            ]
            .iter()
            .any(|needle| content.contains(needle));
            if runtime || command {
                violations.push(name.to_owned());
            }
        }
    }
    violations.sort();
    violations.dedup();
    Ok(json!({
        "schema": "docsight.architecture-check/v1",
        "rust_only": violations.is_empty(),
        "files_checked": count,
        "violations": violations,
    }))
}
