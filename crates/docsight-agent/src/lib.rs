use docsight_core::{Diagnostic, DocumentSource};
use serde::Serialize;

pub const AGENT_SCHEMA: &str = "docsight.agent/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OutputLimits {
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
            limits: OutputLimits { truncated: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AGENT_SCHEMA, AgentEnvelope};
    use docsight_core::{DocsightError, DocumentSource};
    use serde::Serialize;

    #[derive(Serialize)]
    struct ResultValue {
        pages: u32,
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
}
