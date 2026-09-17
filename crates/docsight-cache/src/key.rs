use docsight_core::{DocsightError, DocumentFormat, DocumentSource, IR_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const KEY_SCHEMA: &str = "docsight.cache-key/v1";
const DOCUMENT_IR_ARTIFACT: &str = "document-ir";
const MAX_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineIdentity {
    pub executable_sha256: String,
    pub engine_version: String,
    pub ir_schema_version: String,
    pub layout_profile: String,
    pub layout_font_fingerprint: String,
}

impl EngineIdentity {
    pub fn for_executable(
        executable: &Path,
        layout_profile: &str,
        layout_font_fingerprint: &str,
    ) -> Result<Self, DocsightError> {
        Ok(Self {
            executable_sha256: file_sha256(executable)?,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            ir_schema_version: IR_SCHEMA_VERSION.to_owned(),
            layout_profile: layout_profile.to_owned(),
            layout_font_fingerprint: layout_font_fingerprint.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheKey {
    pub schema: String,
    pub artifact: String,
    pub document_sha256: String,
    pub document_format: DocumentFormat,
    pub engine: EngineIdentity,
}

impl CacheKey {
    pub fn document_ir(source: &DocumentSource, engine: &EngineIdentity) -> Self {
        Self {
            schema: KEY_SCHEMA.to_owned(),
            artifact: DOCUMENT_IR_ARTIFACT.to_owned(),
            document_sha256: source.sha256().to_owned(),
            document_format: source.format(),
            engine: engine.clone(),
        }
    }

    pub fn digest(&self) -> Result<String, DocsightError> {
        let bytes = serde_json::to_vec(self).map_err(|error| DocsightError::BackendFailure {
            backend: "docsight-cache".to_owned(),
            message: format!("cache key could not be serialized: {error}"),
        })?;
        Ok(hex(&Sha256::digest(bytes)))
    }

    pub fn is_well_formed(&self) -> bool {
        self.schema == KEY_SCHEMA
            && self.artifact == DOCUMENT_IR_ARTIFACT
            && is_sha256(&self.document_sha256)
            && is_sha256(&self.engine.executable_sha256)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn file_sha256(path: &Path) -> Result<String, DocsightError> {
    let io_error = |source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = File::open(path).map_err(io_error)?;
    let mut reader = file.take(MAX_EXECUTABLE_BYTES + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_EXECUTABLE_BYTES {
            return Err(DocsightError::ResourceLimit {
                resource: "cache engine executable bytes".to_owned(),
                limit: MAX_EXECUTABLE_BYTES,
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}
