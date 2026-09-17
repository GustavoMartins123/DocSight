use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, ToolError>;
pub const MAX_JSON_BYTES: u64 = 2_097_152;
pub const MAX_FILE_BYTES: u64 = 268_435_456;

#[derive(Debug, Serialize)]
pub struct ToolError {
    pub schema: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl ToolError {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            schema: "docsight.tooling-error/v1",
            code,
            message,
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Self::new("OUTPUT_EXISTS", "Output already exists")
        } else {
            Self::new("IO_ERROR", "File or process operation failed")
        }
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(error: serde_json::Error) -> Self {
        if error.to_string().contains("duplicate JSON key") {
            Self::new("DUPLICATE_JSON_KEY", "JSON contains duplicate object keys")
        } else {
            Self::new("INVALID_JSON", "Input is not valid bounded JSON")
        }
    }
}

pub fn require(condition: bool, code: &'static str, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(ToolError::new(code, message))
    }
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct UniqueVisitor;

        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("finite JSON without duplicate keys")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(value)))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(value.into())))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
                Number::from_f64(value)
                    .map(|number| UniqueValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value.to_owned())))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(value)))
            }

            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? { values.push(value); }
                Ok(UniqueValue(Value::Array(values)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, UniqueValue(value))) = entries.next_entry::<String, UniqueValue>()? {
                    if values.insert(key, value).is_some() { return Err(serde::de::Error::custom("duplicate JSON key")); }
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }

        deserializer.deserialize_any(UniqueVisitor)
    }
}

pub fn parse_json(bytes: &[u8]) -> Result<Value> { Ok(serde_json::from_slice::<UniqueValue>(bytes)?.0) }

pub fn read_bytes(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    require(metadata.is_file(), "INVALID_FILE", "Input must be a regular file without symlinks")?;
    require(metadata.len() <= maximum, "FILE_SIZE_LIMIT", "Input exceeds the size limit")?;
    let limit = maximum.checked_add(1).ok_or_else(|| ToolError::new("INVALID_LIMIT", "Size limit overflows"))?;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    require(u64::try_from(bytes.len()).is_ok_and(|length| length <= maximum), "FILE_SIZE_LIMIT", "Input grew beyond the size limit")?;
    Ok(bytes)
}

pub fn read_json(path: &Path) -> Result<Value> { parse_json(&read_bytes(path, MAX_JSON_BYTES)?) }

pub fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let pretty = serde_json::to_string_pretty(&serde_json::to_value(value)?)?;
    let mut ascii = String::with_capacity(pretty.len() + 1);
    for character in pretty.chars() {
        if character.is_ascii() && character != '\u{7f}' { ascii.push(character); }
        else {
            let mut units = [0u16; 2];
            for unit in character.encode_utf16(&mut units) {
                use std::fmt::Write;
                write!(ascii, "\\u{:04x}", *unit).map_err(|_| ToolError::new("JSON_ENCODING", "Cannot encode canonical JSON"))?;
            }
        }
    }
    ascii.push('\n');
    Ok(ascii.into_bytes())
}

pub fn write_new(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let parent = match path.parent() { Some(parent) if !parent.as_os_str().is_empty() => parent, _ => Path::new("."), };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o600 };
        temporary.as_file().set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    require(!executable || !bytes.is_empty(), "INVALID_FILE", "Executable output must not be empty")?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|error| ToolError::from(error.error))?;
    Ok(())
}

pub fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

pub fn sha256_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    require(metadata.is_file(), "INVALID_FILE", "Digest input must be a regular file")?;
    require(metadata.len() <= 536_870_912, "FILE_SIZE_LIMIT", "Digest input exceeds its byte limit")?;
    let mut file = File::open(path)?;
    let mut total = 0u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65_536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 { break; }
        total = total.checked_add(length as u64).ok_or_else(|| ToolError::new("FILE_SIZE_LIMIT", "Digest input size overflow"))?;
        require(total <= 536_870_912, "FILE_SIZE_LIMIT", "Digest input grew beyond its byte limit")?;
        hasher.update(&buffer[..length]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn checked_revision(value: &str) -> Result<()> {
    require(is_hex(value, 40), "INVALID_REVISION", "Revision must be a full lowercase Git commit SHA")
}

pub fn safe_member(value: &str) -> Result<&str> {
    let valid = !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
        && value.split('/').all(|part| {
            let name = part.split('.').next().unwrap_or_default().to_ascii_uppercase();
            !part.is_empty() && part != "." && part != ".." && !part.ends_with('.')
                && !["CON", "PRN", "AUX", "NUL"].contains(&name.as_str())
                && !(name.len() == 4 && (name.starts_with("COM") || name.starts_with("LPT")) && matches!(name.as_bytes()[3], b'1'..=b'9'))
        });
    require(valid, "UNSAFE_ARCHIVE_PATH", "Path must be portable, relative and contained")?;
    Ok(value)
}

pub fn contained_file(root: &Path, relative: &str) -> Result<PathBuf> {
    safe_member(relative)?;
    let base = root.canonicalize()?;
    let mut selected = base.clone();
    for part in relative.split('/') {
        selected.push(part);
        require(!fs::symlink_metadata(&selected)?.file_type().is_symlink(), "INVALID_FILE", "Symlinks are not permitted")?;
    }
    require(selected.is_file() && selected.canonicalize()?.starts_with(&base), "INVALID_FILE", "Input must be a contained regular file")?;
    Ok(selected)
}

pub fn workspace_root() -> PathBuf { { let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); root.pop(); root } }
pub fn workspace_version() -> &'static str { env!("CARGO_PKG_VERSION") }
pub fn read_json_limit(path: &Path, limit: u64) -> Result<Value> { parse_json(&read_bytes(path, limit)?) }
pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> { serde_json::from_value(value).map_err(|_| ToolError::new("INVALID_FIELDS", "Input fields or field types are invalid")) }
pub fn text(bytes: &[u8]) -> Result<&str> { std::str::from_utf8(bytes).map_err(|_| ToolError::new("INVALID_UTF8", "Input must contain valid UTF-8")) }
pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> { value.get(key).ok_or_else(|| ToolError::new("INVALID_FIELDS", "A required field is missing")) }
pub fn string(value: &Value) -> Result<&str> { value.as_str().ok_or_else(|| ToolError::new("INVALID_FIELDS", "Expected a string")) }
pub fn array(value: &Value) -> Result<&[Value]> { value.as_array().map(Vec::as_slice).ok_or_else(|| ToolError::new("INVALID_FIELDS", "Expected an array")) }
pub fn exact_keys(value: &Value, keys: &[&str]) -> Result<()> { require(value.as_object().is_some_and(|object| object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))), "INVALID_FIELDS", "Input has missing or unknown fields") }
pub fn bounded_integer(value: &Value, minimum: u64, maximum: u64) -> Result<u64> { value.as_u64().filter(|n| *n >= minimum && *n <= maximum).ok_or_else(|| ToolError::new("INVALID_INTEGER", "Integer is outside its permitted range")) }
pub fn finite_number(value: &Value, minimum: f64, maximum: f64) -> Result<f64> { value.as_f64().filter(|n| n.is_finite() && *n >= minimum && *n <= maximum).ok_or_else(|| ToolError::new("INVALID_NUMBER", "Number must be finite and within its limits")) }

pub fn checked_version(version: &str) -> Result<()> {
    let (base, suffix) = match version.split_once('-') { Some((base, suffix)) => (base, Some(suffix)), None => (version, None), };
    let parts: Vec<_> = base.split('.').collect();
    let valid = parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()) && (part.len() == 1 || !part.starts_with('0')))
        && suffix.is_none_or(|suffix| !suffix.is_empty() && suffix.split(['.', '-']).all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric())));
    require(valid, "INVALID_VERSION", "Version must be a canonical semantic release version")
}

pub fn checked_digest(value: &str) -> Result<()> { require(is_hex(value, 64), "INVALID_DIGEST", "Expected a lowercase SHA-256 digest") }
pub fn is_code(value: &str) -> bool { (3..=80).contains(&value.len()) && value.as_bytes()[0].is_ascii_uppercase() && value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') }

pub fn no_symlinks(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        require(!matches!(component, std::path::Component::ParentDir), "INVALID_PATH", "Parent traversal is not permitted")?;
        current.push(component.as_os_str());
        if matches!(component, std::path::Component::Prefix(_)) { continue; }
        let metadata = fs::symlink_metadata(&current)?;
        require(!metadata.file_type().is_symlink(), "INVALID_FILE", "Symlinks are not permitted")?;
    }
    Ok(())
}

pub fn list_files(root: &Path, maximum: usize) -> Result<Vec<PathBuf>> {
    no_symlinks(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut count = 0usize;
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(directory)? {
            let item = item?;
            count = count.checked_add(1).ok_or_else(|| ToolError::new("RESOURCE_LIMIT", "Resource count overflow"))?;
            require(count <= maximum, "RESOURCE_LIMIT", "Directory traversal exceeds its limit")?;
            let kind = item.file_type()?;
            require(!kind.is_symlink(), "INVALID_FILE", "Directory resources cannot follow symlinks")?;
            if kind.is_dir() { pending.push(item.path()); }
            else if kind.is_file() { files.push(item.path()); }
            else { return Err(ToolError::new("INVALID_FILE", "Special files are not supported")); }
        }
    }
    files.sort();
    Ok(files)
}

pub fn flat_files(root: &Path, extension: &str, maximum: usize) -> Result<Vec<PathBuf>> {
    no_symlinks(root)?;
    let mut files = Vec::new();
    for (index, item) in fs::read_dir(root)?.enumerate() {
        require(index < maximum, "RESOURCE_LIMIT", "Directory exceeds its entry limit")?;
        let item = item?;
        if item.path().extension().is_some_and(|value| value == extension) {
            require(item.file_type()?.is_file(), "INVALID_FILE", "Input must be a regular file")?;
            files.push(item.path());
        }
    }
    files.sort();
    Ok(files)
}

pub fn required_option<'de, T: Deserialize<'de>, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<T>, D::Error> { Option::<T>::deserialize(deserializer) }
