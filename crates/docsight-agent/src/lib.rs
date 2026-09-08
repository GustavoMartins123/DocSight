use docsight_core::{Diagnostic, DocsightError, DocumentSource};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

pub const AGENT_SCHEMA: &str = "docsight.agent/v2";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentReference {
    pub id: String,
    pub sha256: String,
}

impl From<&DocumentSource> for DocumentReference {
    fn from(source: &DocumentSource) -> Self {
        Self {
            id: source.id(),
            sha256: source.sha256().to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OutputLimits {
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub text_truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub warnings_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_warnings: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_warnings: Option<usize>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentEnvelope<T>
where
    T: Serialize,
{
    pub schema: &'static str,
    pub engine: &'static str,
    pub document: DocumentReference,
    pub result: T,
    pub warnings: Vec<Diagnostic>,
    pub limits: OutputLimits,
}

impl<T> AgentEnvelope<T>
where
    T: Serialize,
{
    pub fn complete(source: &DocumentSource, result: T) -> Self {
        Self::with_warnings(source, result, Vec::new())
    }

    pub fn with_warnings(source: &DocumentSource, result: T, warnings: Vec<Diagnostic>) -> Self {
        Self {
            schema: AGENT_SCHEMA,
            engine: env!("CARGO_PKG_VERSION"),
            document: source.into(),
            result,
            warnings,
            limits: OutputLimits::default(),
        }
    }

    pub fn with_limits(
        source: &DocumentSource,
        result: T,
        warnings: Vec<Diagnostic>,
        limits: OutputLimits,
    ) -> Self {
        Self {
            schema: AGENT_SCHEMA,
            engine: env!("CARGO_PKG_VERSION"),
            document: source.into(),
            result,
            warnings,
            limits,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentErrorRecord {
    pub code: String,
    pub exit_code: u8,
    pub message: String,
    pub effect: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentErrorEnvelope {
    pub schema: &'static str,
    pub engine: &'static str,
    pub error: AgentErrorRecord,
}

impl AgentErrorEnvelope {
    pub fn from_error(error: &DocsightError) -> Self {
        let diag = error.diagnostic();
        let object = match error {
            DocsightError::ObjectNotFound { object } => Some(object.clone()),
            _ => None,
        };
        Self {
            schema: AGENT_SCHEMA,
            engine: env!("CARGO_PKG_VERSION"),
            error: AgentErrorRecord {
                code: diag.code,
                exit_code: error.exit_code(),
                message: diag.message,
                effect: diag.effect,
                object,
            },
        }
    }
}

pub struct ContinuationToken;

impl ContinuationToken {
    pub fn encode(command: &str, offset: usize, sha256: &str) -> String {
        let prefix = if sha256.len() > 16 {
            &sha256[..16]
        } else {
            sha256
        };
        let raw = format!("{command}:{offset}:{prefix}");
        raw.as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    }

    pub fn decode(
        token: &str,
        expected_command: &str,
        expected_sha256: &str,
    ) -> Result<usize, DocsightError> {
        if token.is_empty() || token.len() % 2 != 0 {
            return Err(DocsightError::InvalidArgument {
                message: "continuation token format is invalid".to_owned(),
            });
        }

        let bytes = (0..token.len())
            .step_by(2)
            .map(|i| {
                let chunk = &token[i..i + 2];
                u8::from_str_radix(chunk, 16).map_err(|_| DocsightError::InvalidArgument {
                    message: "continuation token contains invalid hex characters".to_owned(),
                })
            })
            .collect::<Result<Vec<u8>, DocsightError>>()?;

        let raw = String::from_utf8(bytes).map_err(|_| DocsightError::InvalidArgument {
            message: "continuation token is not valid UTF-8".to_owned(),
        })?;

        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 3 {
            return Err(DocsightError::InvalidArgument {
                message: "continuation token payload is invalid".to_owned(),
            });
        }

        let cmd = parts[0];
        let off_str = parts[1];
        let digest_prefix = parts[2];

        if cmd != expected_command {
            return Err(DocsightError::InvalidArgument {
                message: format!(
                    "continuation token was created for '{cmd}' but received for '{expected_command}'"
                ),
            });
        }

        if !expected_sha256.starts_with(digest_prefix) {
            return Err(DocsightError::InvalidArgument {
                message: "continuation token does not match the target document".to_owned(),
            });
        }

        off_str
            .parse::<usize>()
            .map_err(|_| DocsightError::InvalidArgument {
                message: "continuation token offset is not a valid integer".to_owned(),
            })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryLimits {
    pub max_bytes: Option<usize>,
    pub max_items: Option<usize>,
    pub text_limit: Option<usize>,
    pub continue_token: Option<String>,
    pub select: Option<Vec<String>>,
}

pub fn apply_text_limit(text: &str, limit: Option<usize>) -> (String, bool) {
    if let Some(max) = limit {
        if text.chars().count() > max {
            let truncated: String = text.chars().take(max).collect();
            return (truncated, true);
        }
    }
    (text.to_owned(), false)
}

pub fn truncate_json_text_fields(val: &mut serde_json::Value, max_len: usize) -> bool {
    match val {
        serde_json::Value::String(_) => false,
        serde_json::Value::Array(arr) => {
            let mut truncated = false;
            for item in arr {
                truncated = truncate_json_text_fields(item, max_len) || truncated;
            }
            truncated
        }
        serde_json::Value::Object(map) => {
            let mut truncated = false;
            for (key, value) in map {
                if is_text_field(key) {
                    if let serde_json::Value::String(text) = value {
                        if text.chars().count() > max_len {
                            *text = text.chars().take(max_len).collect();
                            truncated = true;
                        }
                    } else {
                        truncated = truncate_json_text_fields(value, max_len) || truncated;
                    }
                } else if matches!(
                    value,
                    serde_json::Value::Array(_) | serde_json::Value::Object(_)
                ) {
                    truncated = truncate_json_text_fields(value, max_len) || truncated;
                }
            }
            truncated
        }
        _ => false,
    }
}

fn is_text_field(field: &str) -> bool {
    matches!(
        field,
        "text" | "text_fragment" | "alt_text" | "caption" | "label" | "details"
    )
}

pub fn project_json(val: &serde_json::Value, select: &[String]) -> serde_json::Value {
    if select.is_empty() {
        return val.clone();
    }
    match val {
        serde_json::Value::Object(map) => {
            let direct_matches: Vec<&String> =
                select.iter().filter(|k| map.contains_key(*k)).collect();
            if !direct_matches.is_empty() {
                let mut projected = serde_json::Map::new();
                for key in select {
                    if let Some(v) = map.get(key) {
                        projected.insert(key.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(projected)
            } else {
                let mut projected = serde_json::Map::new();
                for (k, v) in map {
                    if let serde_json::Value::Array(arr) = v {
                        let inner_projected: Vec<serde_json::Value> =
                            arr.iter().map(|item| project_json(item, select)).collect();
                        projected.insert(k.clone(), serde_json::Value::Array(inner_projected));
                    } else {
                        projected.insert(k.clone(), v.clone());
                    }
                }
                serde_json::Value::Object(projected)
            }
        }
        serde_json::Value::Array(arr) => {
            let projected: Vec<serde_json::Value> =
                arr.iter().map(|item| project_json(item, select)).collect();
            serde_json::Value::Array(projected)
        }
        other => other.clone(),
    }
}

pub fn validate_projection(
    val: &serde_json::Value,
    select: &[String],
) -> Result<(), DocsightError> {
    if select.is_empty() {
        return Ok(());
    }
    let unknown: Vec<&String> = select
        .iter()
        .filter(|key| !value_contains_key(val, key))
        .collect();
    if !unknown.is_empty() {
        let names: Vec<String> = unknown.iter().map(|key| key.to_string()).collect();
        return Err(DocsightError::InvalidArgument {
            message: format!(
                "--select fields do not exist in the result: {}",
                names.join(", ")
            ),
        });
    }
    Ok(())
}

fn value_contains_key(val: &serde_json::Value, key: &str) -> bool {
    match val {
        serde_json::Value::Object(map) => {
            if map.contains_key(key) {
                return true;
            }
            map.values().any(|value| value_contains_key(value, key))
        }
        serde_json::Value::Array(arr) => arr.iter().any(|item| value_contains_key(item, key)),
        _ => false,
    }
}

pub fn apply_collection_limits<T: Clone>(
    items: &[T],
    limits: &QueryLimits,
    command: &str,
    sha256: &str,
) -> Result<(Vec<T>, OutputLimits), DocsightError> {
    let start_offset = if let Some(ref token) = limits.continue_token {
        ContinuationToken::decode(token, command, sha256)?
    } else {
        0
    };

    let total_items = items.len();
    if start_offset >= total_items {
        return Ok((
            Vec::new(),
            OutputLimits {
                truncated: false,
                text_truncated: false,
                warnings_truncated: false,
                continuation_token: None,
                total_items: Some(total_items),
                returned_items: Some(0),
                total_warnings: None,
                returned_warnings: None,
            },
        ));
    }

    let available = &items[start_offset..];
    let count = if let Some(max_items) = limits.max_items {
        available.len().min(max_items)
    } else {
        available.len()
    };

    let selected: Vec<T> = available[..count].to_vec();
    let next_offset = start_offset + count;
    let truncated = next_offset < total_items;
    let continuation_token = if truncated {
        Some(ContinuationToken::encode(command, next_offset, sha256))
    } else {
        None
    };

    Ok((
        selected,
        OutputLimits {
            truncated,
            text_truncated: false,
            warnings_truncated: false,
            continuation_token,
            total_items: Some(total_items),
            returned_items: Some(count),
            total_warnings: None,
            returned_warnings: None,
        },
    ))
}

pub fn apply_bounded_collection<T: Clone + Serialize, F>(
    items: &[T],
    limits: &QueryLimits,
    command: &str,
    source: &DocumentSource,
    warnings: Vec<Diagnostic>,
    wrap_result: F,
) -> Result<AgentEnvelope<serde_json::Value>, DocsightError>
where
    F: Fn(Vec<T>) -> Result<serde_json::Value, DocsightError>,
{
    let start_offset = if let Some(ref token) = limits.continue_token {
        ContinuationToken::decode(token, command, source.sha256())?
    } else {
        0
    };

    let total_items = items.len();
    let available = if start_offset < total_items {
        &items[start_offset..]
    } else {
        &[]
    };

    let mut count = if let Some(max_items) = limits.max_items {
        available.len().min(max_items)
    } else {
        available.len()
    };

    'items: loop {
        let current_slice = available[..count].to_vec();
        let mut val = wrap_result(current_slice)?;
        let mut text_truncated = false;
        if let Some(text_limit) = limits.text_limit {
            text_truncated = truncate_json_text_fields(&mut val, text_limit);
        }
        if let Some(ref select) = limits.select {
            validate_projection(&val, select)?;
            val = project_json(&val, select);
        }

        let next_offset = start_offset + count;
        let truncated = next_offset < total_items;
        let continuation_token = if truncated {
            Some(ContinuationToken::encode(
                command,
                next_offset,
                source.sha256(),
            ))
        } else {
            None
        };

        let total_warnings = warnings.len();
        let mut selected_warnings = warnings.clone();
        let mut warnings_truncated = false;
        loop {
            let output_limits = OutputLimits {
                truncated,
                text_truncated,
                warnings_truncated,
                continuation_token: continuation_token.clone(),
                total_items: Some(total_items),
                returned_items: Some(count),
                total_warnings: warnings_truncated.then_some(total_warnings),
                returned_warnings: warnings_truncated.then_some(selected_warnings.len()),
            };
            let envelope = AgentEnvelope::with_limits(
                source,
                val.clone(),
                selected_warnings.clone(),
                output_limits,
            );

            if let Some(max_bytes) = limits.max_bytes {
                let serialized = serde_json::to_string(&envelope).map_err(|e| {
                    DocsightError::MalformedDocument {
                        message: e.to_string(),
                    }
                })?;
                if serialized.len().saturating_add(1) > max_bytes {
                    if selected_warnings.pop().is_some() {
                        warnings_truncated = true;
                        continue;
                    }
                    if count > 0 {
                        count -= 1;
                        continue 'items;
                    }
                    return Err(DocsightError::InvalidArgument {
                        message: format!(
                            "--max-bytes {max_bytes} is smaller than the minimum agent envelope; increase the cap or use --select to reduce the payload"
                        ),
                    });
                }
            }

            return Ok(envelope);
        }
    }
}

pub struct NdjsonWriter<W: Write> {
    writer: W,
    seq: u64,
    limits: QueryLimits,
    command: String,
    sha256: String,
    bytes_written: usize,
    items_emitted: usize,
    truncated: bool,
    continuation_offset: usize,
    total_items: usize,
    text_truncated: bool,
    warnings_seen: usize,
    warnings_emitted: usize,
    page_end_reserve: Option<usize>,
}

impl<W: Write> NdjsonWriter<W> {
    pub fn new(
        writer: W,
        limits: QueryLimits,
        command: String,
        sha256: String,
        total_items: usize,
    ) -> Result<Self, DocsightError> {
        let continuation_offset = if let Some(ref token) = limits.continue_token {
            ContinuationToken::decode(token, &command, &sha256)?
        } else {
            0
        };

        Ok(Self {
            writer,
            seq: 0,
            limits,
            command,
            sha256,
            bytes_written: 0,
            items_emitted: 0,
            truncated: false,
            continuation_offset,
            total_items,
            text_truncated: false,
            warnings_seen: 0,
            warnings_emitted: 0,
            page_end_reserve: None,
        })
    }

    pub fn continuation_offset(&self) -> usize {
        self.continuation_offset
    }

    fn serialized_line(val: &serde_json::Value) -> Result<String, DocsightError> {
        serde_json::to_string(val).map_err(|e| DocsightError::MalformedDocument {
            message: e.to_string(),
        })
    }

    fn write_serialized_line(&mut self, line: &str) -> Result<(), DocsightError> {
        let bytes = line.len() + 1;
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .map_err(|e| DocsightError::Io {
                path: PathBuf::from("stdout"),
                source: e,
            })?;
        self.bytes_written += bytes;
        Ok(())
    }

    fn maximum_done_bytes(&self) -> Result<usize, DocsightError> {
        let continuation_token = if self.total_items > 0 {
            Some(ContinuationToken::encode(
                &self.command,
                self.total_items.saturating_sub(1),
                &self.sha256,
            ))
        } else {
            None
        };
        let limits = OutputLimits {
            truncated: true,
            text_truncated: self.text_truncated,
            warnings_truncated: false,
            continuation_token: continuation_token.clone(),
            total_items: Some(self.total_items),
            returned_items: Some(self.total_items),
            total_warnings: None,
            returned_warnings: None,
        };
        let line = serde_json::json!({
            "seq": u64::MAX,
            "type": "done",
            "limits": limits,
        });
        let warning_limits = OutputLimits {
            truncated: true,
            text_truncated: self.text_truncated,
            warnings_truncated: true,
            continuation_token,
            total_items: Some(self.total_items),
            returned_items: Some(self.total_items),
            total_warnings: Some(self.warnings_seen),
            returned_warnings: Some(self.warnings_emitted),
        };
        let warning_line = serde_json::json!({
            "seq": u64::MAX,
            "type": "done",
            "limits": warning_limits,
        });
        Ok(Self::serialized_line(&line)
            .map(|value| value.len() + 1)?
            .max(Self::serialized_line(&warning_line)?.len() + 1))
    }

    fn fits(
        &self,
        additional_bytes: usize,
        additional_reserve: usize,
    ) -> Result<bool, DocsightError> {
        let Some(max_bytes) = self.limits.max_bytes else {
            return Ok(true);
        };
        let reserved = self.page_end_reserve.unwrap_or(0);
        let done = self.maximum_done_bytes()?;
        let required = self
            .bytes_written
            .checked_add(additional_bytes)
            .and_then(|value| value.checked_add(additional_reserve))
            .and_then(|value| value.checked_add(reserved))
            .and_then(|value| value.checked_add(done))
            .ok_or_else(|| DocsightError::ResourceLimit {
                resource: "NDJSON output bytes".to_owned(),
                limit: max_bytes as u64,
            })?;
        Ok(required <= max_bytes)
    }

    fn write_required_line(&mut self, val: &serde_json::Value) -> Result<(), DocsightError> {
        let line = Self::serialized_line(val)?;
        if !self.fits(line.len() + 1, 0)? {
            let max_bytes = self.limits.max_bytes.unwrap_or(0);
            return Err(DocsightError::InvalidArgument {
                message: format!(
                    "--max-bytes {max_bytes} is smaller than the minimum NDJSON envelope"
                ),
            });
        }
        self.write_serialized_line(&line)
    }

    fn write_optional_line(&mut self, val: &serde_json::Value) -> Result<bool, DocsightError> {
        let line = Self::serialized_line(val)?;
        if !self.fits(line.len() + 1, 0)? {
            self.truncated = true;
            return Ok(false);
        }
        self.write_serialized_line(&line)?;
        Ok(true)
    }

    pub fn write_meta(&mut self, doc_ref: &DocumentReference) -> Result<(), DocsightError> {
        self.seq += 1;
        let line = serde_json::json!({
            "seq": self.seq,
            "type": "meta",
            "schema": AGENT_SCHEMA,
            "engine": env!("CARGO_PKG_VERSION"),
            "document": doc_ref,
        });
        self.write_required_line(&line)
    }

    pub fn write_page_begin(&mut self, page: u32) -> Result<bool, DocsightError> {
        let begin_seq = self.seq.saturating_add(1);
        let line = serde_json::json!({
            "seq": begin_seq,
            "type": "page.begin",
            "page": page,
        });
        let end = serde_json::json!({
            "seq": u64::MAX,
            "type": "page.end",
            "page": page,
        });
        let begin_line = Self::serialized_line(&line)?;
        let end_reserve = Self::serialized_line(&end)?.len() + 1;
        if !self.fits(begin_line.len() + 1, end_reserve)? {
            self.truncated = true;
            return Ok(false);
        }
        self.seq = begin_seq;
        self.write_serialized_line(&begin_line)?;
        self.page_end_reserve = Some(end_reserve);
        Ok(true)
    }

    pub fn write_page_end(&mut self, page: u32) -> Result<bool, DocsightError> {
        let Some(reserved) = self.page_end_reserve.take() else {
            return Ok(false);
        };
        self.seq += 1;
        let line = serde_json::json!({
            "seq": self.seq,
            "type": "page.end",
            "page": page,
        });
        let serialized = Self::serialized_line(&line)?;
        if serialized.len() + 1 > reserved {
            return Err(DocsightError::ResourceLimit {
                resource: "NDJSON page boundary bytes".to_owned(),
                limit: reserved as u64,
            });
        }
        self.write_serialized_line(&serialized)?;
        Ok(true)
    }

    pub fn write_warning(&mut self, diag: &Diagnostic) -> Result<bool, DocsightError> {
        self.warnings_seen += 1;
        let seq = self.seq.saturating_add(1);
        let line = serde_json::json!({
            "seq": seq,
            "type": "warning",
            "diagnostic": diag,
        });
        if self.write_optional_line(&line)? {
            self.seq = seq;
            self.warnings_emitted += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn write_item(
        &mut self,
        item_type: &str,
        data: &serde_json::Value,
    ) -> Result<bool, DocsightError> {
        if self.truncated {
            return Ok(false);
        }

        if let Some(max_items) = self.limits.max_items {
            if self.items_emitted >= max_items {
                self.truncated = true;
                return Ok(false);
            }
        }

        let mut projected = if let Some(ref fields) = self.limits.select {
            validate_projection(data, fields)?;
            project_json(data, fields)
        } else {
            data.clone()
        };

        if let Some(text_limit) = self.limits.text_limit {
            self.text_truncated =
                truncate_json_text_fields(&mut projected, text_limit) || self.text_truncated;
        }

        let obj = match projected {
            serde_json::Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("value".to_owned(), other);
                map
            }
        };

        let seq = self.seq.saturating_add(1);
        let mut ordered_obj = serde_json::Map::new();
        ordered_obj.insert("seq".to_owned(), serde_json::json!(seq));
        ordered_obj.insert("type".to_owned(), serde_json::json!(item_type));
        for (k, v) in obj {
            ordered_obj.insert(k, v);
        }

        let line_val = serde_json::Value::Object(ordered_obj);
        let line_str =
            serde_json::to_string(&line_val).map_err(|e| DocsightError::MalformedDocument {
                message: e.to_string(),
            })?;
        let line_bytes = line_str.len() + 1;

        if !self.fits(line_bytes, 0)? {
            self.truncated = true;
            return Ok(false);
        }

        self.writer
            .write_all(line_str.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .map_err(|e| DocsightError::Io {
                path: PathBuf::from("stdout"),
                source: e,
            })?;

        self.bytes_written += line_bytes;
        self.seq = seq;
        self.items_emitted += 1;
        self.continuation_offset += 1;

        Ok(true)
    }

    pub fn finish(mut self) -> Result<OutputLimits, DocsightError> {
        let truncated = self.truncated || self.continuation_offset < self.total_items;
        let continuation_token = if truncated && self.continuation_offset < self.total_items {
            Some(ContinuationToken::encode(
                &self.command,
                self.continuation_offset,
                &self.sha256,
            ))
        } else {
            None
        };

        self.seq += 1;
        let limits = OutputLimits {
            truncated,
            text_truncated: self.text_truncated,
            warnings_truncated: self.warnings_emitted < self.warnings_seen,
            continuation_token,
            total_items: Some(self.total_items),
            returned_items: Some(self.items_emitted),
            total_warnings: (self.warnings_emitted < self.warnings_seen)
                .then_some(self.warnings_seen),
            returned_warnings: (self.warnings_emitted < self.warnings_seen)
                .then_some(self.warnings_emitted),
        };
        let line = serde_json::json!({
            "seq": self.seq,
            "type": "done",
            "limits": limits,
        });
        let serialized = Self::serialized_line(&line)?;
        if let Some(max_bytes) = self.limits.max_bytes {
            let projected = self
                .bytes_written
                .checked_add(serialized.len() + 1)
                .ok_or_else(|| DocsightError::ResourceLimit {
                    resource: "NDJSON output bytes".to_owned(),
                    limit: max_bytes as u64,
                })?;
            if projected > max_bytes {
                return Err(DocsightError::ResourceLimit {
                    resource: "NDJSON output bytes".to_owned(),
                    limit: max_bytes as u64,
                });
            }
        }
        self.write_serialized_line(&serialized)?;
        Ok(limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docsight_core::DocumentSource;
    use serde::Serialize;

    #[derive(Clone, Serialize)]
    struct ResultValue {
        pages: u32,
    }

    #[derive(Clone, Serialize)]
    struct TestItem {
        id: String,
        text: String,
    }

    #[test]
    fn serializes_fields_in_contract_order() -> Result<(), Box<dyn std::error::Error>> {
        let source = DocumentSource::from_bytes(b"%PDF-1.7\n".to_vec())?;
        let envelope = AgentEnvelope::complete(&source, ResultValue { pages: 1 });
        let json = serde_json::to_string(&envelope)?;
        assert!(json.starts_with(&format!("{{\"schema\":\"{AGENT_SCHEMA}\",\"engine\":")));
        assert!(json.contains("\"limits\":{\"truncated\":false}"));
        Ok(())
    }

    #[test]
    fn source_errors_remain_typed() {
        let result = DocumentSource::from_bytes(Vec::new());
        assert!(matches!(result, Err(DocsightError::UnsupportedFormat)));
    }

    #[test]
    fn continuation_token_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let token = ContinuationToken::encode("outline", 42, digest);
        let offset = ContinuationToken::decode(&token, "outline", digest)?;
        assert_eq!(offset, 42);

        let mismatch_cmd = ContinuationToken::decode(&token, "page", digest);
        assert!(mismatch_cmd.is_err());

        let mismatch_sha = ContinuationToken::decode(&token, "outline", "ffffffffffffffff");
        assert!(mismatch_sha.is_err());
        Ok(())
    }

    #[test]
    fn query_limits_apply_max_items_and_continuation() -> Result<(), Box<dyn std::error::Error>> {
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let items = vec![
            TestItem {
                id: "1".into(),
                text: "one".into(),
            },
            TestItem {
                id: "2".into(),
                text: "two".into(),
            },
            TestItem {
                id: "3".into(),
                text: "three".into(),
            },
        ];
        let limits = QueryLimits {
            max_items: Some(2),
            ..Default::default()
        };
        let (selected, out_limits) = apply_collection_limits(&items, &limits, "test", digest)?;
        assert_eq!(selected.len(), 2);
        assert!(out_limits.truncated);
        assert!(out_limits.continuation_token.is_some());

        let resume_limits = QueryLimits {
            continue_token: out_limits.continuation_token,
            max_items: Some(2),
            ..Default::default()
        };
        let (resumed, final_limits) =
            apply_collection_limits(&items, &resume_limits, "test", digest)?;
        assert_eq!(resumed.len(), 1);
        assert!(!final_limits.truncated);
        assert!(final_limits.continuation_token.is_none());
        Ok(())
    }

    #[test]
    fn ndjson_stream_emits_sequential_valid_records() -> Result<(), Box<dyn std::error::Error>> {
        let mut buf = Vec::new();
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned();
        let limits = QueryLimits {
            max_items: Some(1),
            ..Default::default()
        };
        let mut writer = NdjsonWriter::new(&mut buf, limits, "outline".into(), digest, 2)?;
        let doc_ref = DocumentReference {
            id: "doc_123".into(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        };
        writer.write_meta(&doc_ref)?;
        let item1 = serde_json::json!({"id": "h_1", "text": "Long title text here"});
        let item2 = serde_json::json!({"id": "h_2", "text": "Second title"});
        assert!(writer.write_item("heading", &item1)?);
        assert!(!writer.write_item("heading", &item2)?);
        let out_limits = writer.finish()?;
        assert!(out_limits.truncated);

        let lines: Vec<&str> = std::str::from_utf8(&buf)?.lines().collect();
        assert_eq!(lines.len(), 3);
        let line1: serde_json::Value = serde_json::from_str(lines[0])?;
        assert_eq!(line1["seq"], 1);
        assert_eq!(line1["type"], "meta");
        let line2: serde_json::Value = serde_json::from_str(lines[1])?;
        assert_eq!(line2["seq"], 2);
        assert_eq!(line2["type"], "heading");
        let line3: serde_json::Value = serde_json::from_str(lines[2])?;
        assert_eq!(line3["seq"], 3);
        assert_eq!(line3["type"], "done");
        assert_eq!(line3["limits"]["truncated"], true);

        Ok(())
    }

    #[test]
    fn field_projection_and_text_limit() {
        let data = serde_json::json!({
            "id": "item_1",
            "text": "Extremely long text that should be capped",
            "score": 42
        });
        let projected = project_json(&data, &["id".into(), "text".into()]);
        assert!(projected.get("id").is_some());
        assert!(projected.get("text").is_some());
        assert!(projected.get("score").is_none());

        let mut projected_copy = projected.clone();
        let truncated = truncate_json_text_fields(&mut projected_copy, 9);
        assert_eq!(projected_copy["text"], "Extremely");
        assert!(truncated);
        assert_eq!(projected_copy["id"], "item_1");
    }

    #[test]
    fn agent_error_envelope_formatting() {
        let err = DocsightError::ObjectNotFound {
            object: "tbl_1".into(),
        };
        let envelope = AgentErrorEnvelope::from_error(&err);
        assert_eq!(envelope.schema, AGENT_SCHEMA);
        assert_eq!(envelope.error.exit_code, 21);
        assert_eq!(envelope.error.code, "OBJECT_NOT_FOUND");
        assert_eq!(envelope.error.object.as_deref(), Some("tbl_1"));
    }
}
