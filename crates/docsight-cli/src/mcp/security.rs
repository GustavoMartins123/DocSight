use docsight_core::DocsightError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub(super) const JOB_ENV: &str = "DOCSIGHT_MCP_REQUEST";
pub(super) const CONTRACT: &str = "docsight.mcp/v2";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Limits {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_document_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_memory_bytes: u64,
    pub cpu_timeout_secs: u64,
    pub wall_timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_bytes: 65_536,
            max_response_bytes: 1_048_576,
            max_document_bytes: 64 * 1024 * 1024,
            max_artifact_bytes: 16 * 1024 * 1024,
            max_memory_bytes: 768 * 1024 * 1024,
            cpu_timeout_secs: 30,
            wall_timeout_secs: 120,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), DocsightError> {
        let l = self;
        if !(1024..=1_048_576).contains(&l.max_request_bytes)
            || !(32_768..=8_388_608).contains(&l.max_response_bytes)
            || !(1..=docsight_core::MAX_INSPECT_BYTES).contains(&l.max_document_bytes)
            || !(1..=64 * 1024 * 1024).contains(&l.max_artifact_bytes)
            || !(64 * 1024 * 1024..=768 * 1024 * 1024).contains(&l.max_memory_bytes)
            || !(1..=30).contains(&l.cpu_timeout_secs)
            || !(1..=120).contains(&l.wall_timeout_secs)
        {
            return Err(usage(
                "MCP policy resource limits are outside the supported ranges",
            ));
        }
        Ok(())
    }

    pub fn job_output_bytes(&self) -> Result<u64, DocsightError> {
        self.max_artifact_bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(self.max_response_bytes as u64))
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or_else(|| usage("MCP worker output limit overflows"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub schema: String,
    pub read_roots: Vec<PathBuf>,
    pub write_root: Option<PathBuf>,
    #[serde(deserialize_with = "tool_set")]
    pub allowed_tools: BTreeSet<String>,
    #[serde(default)]
    pub limits: Limits,
}

fn tool_set<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeSet<String>, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    let set: BTreeSet<String> = values.iter().cloned().collect();
    if set.len() != values.len() {
        return Err(serde::de::Error::custom("duplicate MCP tool permission"));
    }
    Ok(set)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Job {
    pub schema: String,
    pub name: String,
    pub arguments: Value,
    pub limits: Limits,
}

pub(super) fn usage(message: impl Into<String>) -> DocsightError {
    DocsightError::InvalidArgument {
        message: message.into(),
    }
}

pub(super) fn denied(field: &str) -> DocsightError {
    DocsightError::AccessDenied {
        operation: format!("MCP {field}"),
    }
}

pub(super) fn limit(resource: &str, maximum: u64) -> DocsightError {
    DocsightError::ResourceLimit {
        resource: resource.to_owned(),
        limit: maximum,
    }
}

pub(super) fn path_string(path: &Path) -> Result<String, DocsightError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| usage("MCP paths must be valid UTF-8"))
}

pub(super) fn bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, DocsightError> {
    if !std::fs::symlink_metadata(path)
        .map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .is_file()
    {
        return Err(usage(
            "MCP configuration and handoff files must be regular files",
        ));
    }
    let file = std::fs::File::open(path).map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(
        maximum
            .checked_add(1)
            .ok_or_else(|| usage("MCP file limit overflows"))?,
    )
    .read_to_end(&mut bytes)
    .map_err(|source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.len() as u64 > maximum {
        return Err(limit("MCP file bytes", maximum));
    }
    Ok(bytes)
}

impl Policy {
    pub fn validate_credentials(&self, cli: &crate::Cli) -> Result<(), DocsightError> {
        for path in [
            &cli.password_file,
            &cli.password_before_file,
            &cli.password_after_file,
        ]
        .into_iter()
        .flatten()
        {
            let path = path
                .canonicalize()
                .map_err(|_| usage("MCP host credential file is unavailable"))?;
            if !path.is_file()
                || self.read_roots.iter().any(|root| path.starts_with(root))
                || self
                    .write_root
                    .as_ref()
                    .is_some_and(|root| path.starts_with(root))
            {
                return Err(usage(
                    "MCP credential files must be regular files outside input and output roots",
                ));
            }
        }
        Ok(())
    }
    pub fn load(path: &Path, catalog: &[Value]) -> Result<Self, DocsightError> {
        let mut policy: Self = serde_json::from_slice(&bounded_file(path, 65_536)?)
            .map_err(|_| usage("invalid MCP policy; expected docsight.mcp-policy/v1"))?;
        if policy.schema != "docsight.mcp-policy/v1"
            || policy.read_roots.is_empty()
            || policy.allowed_tools.is_empty()
        {
            return Err(usage(
                "MCP policy requires its version, non-empty read_roots and allowed_tools",
            ));
        }
        let base = path
            .parent()
            .ok_or_else(|| usage("MCP policy has no parent"))?;
        for root in &mut policy.read_roots {
            *root = canonical_directory(&base.join(&*root))?;
        }
        policy.read_roots.sort();
        policy.read_roots.dedup();
        policy.write_root = policy
            .write_root
            .map(|root| canonical_directory(&base.join(root)))
            .transpose()?;
        if let Some(output) = &policy.write_root {
            for input in &policy.read_roots {
                if output.starts_with(input) || input.starts_with(output) {
                    return Err(usage("MCP read and write roots must be disjoint"));
                }
            }
        }
        for name in &policy.allowed_tools {
            if !catalog
                .iter()
                .any(|tool| tool["name"].as_str() == Some(name))
            {
                return Err(usage("MCP policy names an unknown tool"));
            }
            if writes_artifact(name) && policy.write_root.is_none() {
                return Err(usage("MCP artifact tools require write_root"));
            }
        }
        policy.limits.validate()?;
        Ok(policy)
    }

    pub fn authorize(
        &self,
        name: &str,
        arguments: &Value,
        catalog: &[Value],
    ) -> Result<Value, DocsightError> {
        if !catalog
            .iter()
            .any(|tool| tool["name"].as_str() == Some(name))
        {
            return Err(usage("unknown MCP tool"));
        }
        if !self.allowed_tools.contains(name) {
            return Err(denied("tool"));
        }
        validate_arguments(name, arguments, catalog)?;
        let mut authorized = arguments.clone();
        for key in ["path", "before", "after", "bundle_path"] {
            if let Some(raw) = arguments.get(key).and_then(Value::as_str) {
                let path = Path::new(raw);
                if !path.is_absolute() {
                    return Err(denied("relative input path"));
                }
                let metadata = std::fs::symlink_metadata(path).map_err(|_| denied("input path"))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(denied("input file type"));
                }
                let canonical = path.canonicalize().map_err(|_| denied("input path"))?;
                let input_allowed = self
                    .read_roots
                    .iter()
                    .any(|root| canonical.starts_with(root));
                let bundle_allowed = name == "replay_bundle"
                    && self
                        .write_root
                        .as_ref()
                        .is_some_and(|root| canonical.parent() == Some(root));
                if !(input_allowed || bundle_allowed) {
                    return Err(denied("input path"));
                }
                authorized[key] = Value::String(path_string(&canonical)?);
            }
        }
        if let Some(raw) = arguments.get("out").and_then(Value::as_str) {
            authorized["out"] = Value::String(path_string(&self.output_path(raw)?)?);
        }
        Ok(authorized)
    }

    pub fn output_path(&self, raw: &str) -> Result<PathBuf, DocsightError> {
        let root = self
            .write_root
            .as_ref()
            .ok_or_else(|| denied("artifact writes"))?;
        let path = Path::new(raw);
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(denied("output traversal"));
        }
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let parent = path.parent().ok_or_else(|| denied("output parent"))?;
        if parent.canonicalize().map_err(|_| denied("output parent"))? != *root {
            return Err(denied("output path"));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| denied("output filename"))?;
        let stem = name
            .split('.')
            .next()
            .ok_or_else(|| denied("output filename"))?
            .to_ascii_uppercase();
        if name.len() > 128
            || !name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || name.ends_with('.')
            || matches!(
                stem.as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            )
        {
            return Err(denied("non-portable output filename"));
        }
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(denied("existing output")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(denied("output metadata")),
        }
        Ok(path)
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, DocsightError> {
    let path = path
        .canonicalize()
        .map_err(|_| usage("MCP policy directory is unavailable"))?;
    if !path.is_dir() || path.parent().is_none() {
        return Err(usage(
            "MCP policy roots must be existing non-root directories",
        ));
    }
    Ok(path)
}

pub(super) fn writes_artifact(name: &str) -> bool {
    matches!(name, "render_crop" | "create_bundle")
}

pub(super) fn validate_arguments(
    name: &str,
    arguments: &Value,
    catalog: &[Value],
) -> Result<(), DocsightError> {
    let tool = catalog
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .ok_or_else(|| usage("unknown MCP tool"))?;
    let args = arguments
        .as_object()
        .ok_or_else(|| usage("MCP tool arguments must be an object"))?;
    let schema = &tool["inputSchema"];
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| usage("invalid MCP tool contract"))?;
    for key in schema["required"]
        .as_array()
        .ok_or_else(|| usage("invalid MCP required contract"))?
    {
        let key = key
            .as_str()
            .ok_or_else(|| usage("invalid MCP required field"))?;
        if !args.contains_key(key) {
            return Err(usage(format!("missing required MCP argument: {key}")));
        }
    }
    for (key, value) in args {
        let property = properties.get(key).ok_or_else(|| {
            usage("unknown MCP argument; credentials belong in host configuration")
        })?;
        let valid = match property["type"].as_str() {
            Some("string") => value.as_str().is_some_and(|text| {
                !text.is_empty() && text.chars().count() <= 8192 && !text.contains('\0')
            }),
            Some("integer") => value.as_u64().is_some_and(|number| {
                property["minimum"]
                    .as_u64()
                    .is_some_and(|min| number >= min)
                    && property["maximum"]
                        .as_u64()
                        .is_some_and(|max| number <= max)
            }),
            Some("boolean") => value.is_boolean(),
            Some("array") => value.as_array().is_some_and(|items| {
                !items.is_empty()
                    && items.len() <= 16
                    && items.iter().all(|item| {
                        item.as_str()
                            .is_some_and(|text| !text.is_empty() && text.len() <= 128)
                    })
            }),
            _ => false,
        };
        if !valid {
            return Err(usage(format!("invalid MCP argument type or bound: {key}")));
        }
        if let Some(items) = value.as_array() {
            let distinct: BTreeSet<String> = items.iter().map(Value::to_string).collect();
            if distinct.len() != items.len() {
                return Err(usage(format!("duplicate MCP argument items: {key}")));
            }
            if let Some(allowed) = property["items"]["enum"].as_array()
                && items.iter().any(|item| !allowed.contains(item))
            {
                return Err(usage(format!("invalid MCP argument items: {key}")));
            }
        }
        if let Some(allowed) = property.get("enum").and_then(Value::as_array)
            && !allowed.contains(value)
        {
            return Err(usage(format!("invalid MCP argument choice: {key}")));
        }
    }
    Ok(())
}

pub(super) struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.limit)
        {
            return Err(std::io::Error::other("MCP response limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Limits, Policy};
    use docsight_core::DocsightError;
    use serde_json::{Value, json};
    use std::io::Write;

    #[test]
    fn published_policy_limits_match_runtime_defaults_and_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        let schema: Value =
            serde_json::from_str(include_str!("../../../../schemas/v2/mcp-policy.json"))?;
        let values = serde_json::to_value(Limits::default())?;
        for (key, value) in values.as_object().ok_or("limits object")? {
            let contract = &schema["properties"]["limits"]["properties"][key];
            assert_eq!(contract["default"], *value, "{key}");
            for boundary in ["minimum", "maximum"] {
                let mut candidate = values.clone();
                candidate[key] = contract[boundary].clone();
                serde_json::from_value::<Limits>(candidate)?.validate()?;
            }
            let minimum = contract["minimum"].as_u64().ok_or("minimum")?;
            let maximum = contract["maximum"].as_u64().ok_or("maximum")?;
            for invalid in [minimum - 1, maximum + 1] {
                let mut candidate = values.clone();
                candidate[key] = json!(invalid);
                assert!(
                    matches!(
                        serde_json::from_value::<Limits>(candidate)?.validate(),
                        Err(DocsightError::InvalidArgument { .. })
                    ),
                    "{key}={invalid}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn policy_rejects_unknown_duplicate_and_overlapping_permissions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        std::fs::create_dir(&input)?;
        std::fs::create_dir(&output)?;
        let base = json!({"schema":"docsight.mcp-policy/v1","read_roots":[input],"write_root":output,"allowed_tools":["inspect_document"]});
        let catalog = super::super::list_tools()?;
        for change in [
            json!({"schema":"docsight.mcp-policy/v0"}),
            json!({"allowed_tools":["inspect_document","inspect_document"]}),
            json!({"allowed_tools":["run_shell"]}),
            json!({"allowed_tools":[]}),
            json!({"read_roots":[]}),
            json!({"write_root":input}),
            json!({"unknown":"ignored"}),
        ] {
            let mut policy = base.clone();
            for (key, value) in change.as_object().ok_or("changes")? {
                policy[key] = value.clone();
            }
            let mut file = tempfile::NamedTempFile::new_in(directory.path())?;
            serde_json::to_writer(file.as_file_mut(), &policy)?;
            file.flush()?;
            assert!(Policy::load(file.path(), &catalog).is_err(), "{change}");
        }
        Ok(())
    }
}
