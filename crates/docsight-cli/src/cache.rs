use crate::{emit_warnings, output_serialization_error, stdout_error};
use docsight_cache::{
    CacheClearReport, CacheConfig, CacheKey, CachePruneReport, CacheStats, CacheVerifyReport,
    DEFAULT_CACHE_MAX_BYTES, DEFAULT_CACHE_MAX_ENTRIES, DocumentCache, EngineIdentity, Lookup,
    QuarantineRecord, decode_entry, encode_entry,
};
use docsight_core::{Diagnostic, DocsightError, Document, DocumentSource};
use serde::Serialize;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const CACHE_HANDOFF_ENV: &str = "DOCSIGHT_CACHE_HANDOFF_DIR";
pub(crate) const CACHE_LAYOUT: &str = "docsight.cache/v1";
pub(crate) const LAYOUT_PROFILE: &str = "agent-fidelity-v1";
pub(crate) const CACHE_FLAGS: &[&str] =
    &["--cache-dir", "--cache-max-bytes", "--cache-max-entries"];
pub(crate) const CACHE_KEY_COMPONENTS: &[&str] = &[
    "document_sha256",
    "document_format",
    "executable_sha256",
    "engine_version",
    "ir_schema_version",
    "layout_profile",
    "layout_font_fingerprint",
];
pub(crate) const CACHE_MAINTENANCE_ACTIONS: &[&str] = &["stats", "verify", "prune", "clear"];
pub(crate) const CACHED_COMMANDS: &[&str] = &[
    "inspect", "outline", "text", "tables", "table", "page", "images", "links", "evidence",
    "coverage", "hit", "query", "find", "overview", "focus", "peek", "context", "resolve",
];

const HANDOFF_KEY_FILE: &str = "key.json";
const HANDOFF_ENTRY_FILE: &str = "entry.dsc";
const HANDOFF_RESULT_FILE: &str = "result.dsc";
const HANDOFF_PARTIAL_FILE: &str = "result.partial";
const MAX_HANDOFF_KEY_BYTES: u64 = 64 * 1024;
const MAX_HANDOFF_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct CacheSettings {
    pub(crate) directory: PathBuf,
    pub(crate) max_bytes: u64,
    pub(crate) max_entries: u64,
}

impl CacheSettings {
    pub(crate) fn new(
        directory: &Path,
        max_bytes: Option<usize>,
        max_entries: Option<u64>,
    ) -> Self {
        Self {
            directory: directory.to_path_buf(),
            max_bytes: max_bytes.map_or(DEFAULT_CACHE_MAX_BYTES, |bytes| bytes as u64),
            max_entries: max_entries.unwrap_or(DEFAULT_CACHE_MAX_ENTRIES),
        }
    }

    fn config(&self) -> CacheConfig {
        CacheConfig {
            directory: self.directory.clone(),
            max_bytes: self.max_bytes,
            max_entries: self.max_entries,
        }
    }

    fn open(&self, executable: &Path) -> Result<DocumentCache, DocsightError> {
        DocumentCache::open(&self.config(), engine_identity(executable)?)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Reporting {
    pub(crate) quiet: bool,
    pub(crate) json_errors: bool,
}

pub(crate) struct DocumentLoader<'a> {
    password: &'a [u8],
    cache: Option<LoaderCache>,
    reporting: Reporting,
}

enum LoaderCache {
    Store(DocumentCache),
    Handoff(HandoffChild),
}

impl<'a> DocumentLoader<'a> {
    pub(crate) fn for_invocation(
        password: &'a [u8],
        settings: Option<&CacheSettings>,
        reporting: Reporting,
    ) -> Result<Self, DocsightError> {
        let handoff = std::env::var_os(CACHE_HANDOFF_ENV);
        let cache = match (handoff, settings) {
            (Some(_), Some(_)) => {
                return Err(DocsightError::InvalidArgument {
                    message: "sandbox workers receive the cache from their parent and cannot open --cache-dir"
                        .to_owned(),
                });
            }
            (Some(directory), None) => Some(LoaderCache::Handoff(HandoffChild::open(
                PathBuf::from(directory),
            )?)),
            (None, Some(settings)) => {
                Some(LoaderCache::Store(settings.open(&running_executable()?)?))
            }
            (None, None) => None,
        };
        Ok(Self {
            password,
            cache,
            reporting,
        })
    }

    pub(crate) fn password(&self) -> &'a [u8] {
        self.password
    }

    pub(crate) fn load(&self, source: &DocumentSource) -> Result<Document, DocsightError> {
        self.load_with(source, || {
            docsight_ingest::ingest_with_password(source, self.password)
        })
    }

    pub(crate) fn load_with(
        &self,
        source: &DocumentSource,
        ingest: impl FnOnce() -> Result<Document, DocsightError>,
    ) -> Result<Document, DocsightError> {
        match &self.cache {
            None => ingest(),
            Some(LoaderCache::Store(cache)) => {
                let key = cache.key_for(source);
                match cache.get(&key)? {
                    Lookup::Hit(document) => return Ok(*document),
                    Lookup::Miss => {}
                    Lookup::Quarantined(record) => warn_quarantined(&record, self.reporting)?,
                }
                let document = ingest()?;
                cache.put(&key, &document)?;
                Ok(document)
            }
            Some(LoaderCache::Handoff(handoff)) => handoff.load(source, ingest),
        }
    }
}

struct HandoffChild {
    directory: PathBuf,
    key: CacheKey,
}

impl HandoffChild {
    fn open(directory: PathBuf) -> Result<Self, DocsightError> {
        let path = directory.join(HANDOFF_KEY_FILE);
        let bytes = read_bounded(&path, MAX_HANDOFF_KEY_BYTES)?.ok_or_else(|| {
            handoff_failure(format!("cache handoff key {} is missing", path.display()))
        })?;
        let key: CacheKey = serde_json::from_slice(&bytes)
            .map_err(|error| handoff_failure(format!("cache handoff key is invalid: {error}")))?;
        if !key.is_well_formed() {
            return Err(handoff_failure("cache handoff key is malformed"));
        }
        Ok(Self { directory, key })
    }

    fn load(
        &self,
        source: &DocumentSource,
        ingest: impl FnOnce() -> Result<Document, DocsightError>,
    ) -> Result<Document, DocsightError> {
        if source.sha256() != self.key.document_sha256
            || source.format() != self.key.document_format
        {
            return ingest();
        }
        let entry = self.directory.join(HANDOFF_ENTRY_FILE);
        if let Some(bytes) = read_bounded(&entry, MAX_HANDOFF_ENTRY_BYTES)? {
            return decode_entry(&bytes, &self.key).map_err(|rejection| {
                handoff_failure(format!(
                    "cache handoff entry was rejected: {}",
                    rejection.code()
                ))
            });
        }
        let document = ingest()?;
        self.write_result(&document)?;
        Ok(document)
    }

    fn write_result(&self, document: &Document) -> Result<(), DocsightError> {
        let bytes = encode_entry(&self.key, document)?;
        let partial = self.directory.join(HANDOFF_PARTIAL_FILE);
        let io_error = |source| DocsightError::Io {
            path: partial.clone(),
            source,
        };
        let mut file = create_write_only(&partial).map_err(io_error)?;
        file.write_all(&bytes).map_err(io_error)?;
        drop(file);
        std::fs::rename(&partial, self.directory.join(HANDOFF_RESULT_FILE)).map_err(io_error)
    }
}

pub(crate) struct SandboxCacheHandoff {
    cache: DocumentCache,
    key: CacheKey,
    executable: PathBuf,
    directory: tempfile::TempDir,
    hit: bool,
}

impl SandboxCacheHandoff {
    pub(crate) fn prepare(
        settings: &CacheSettings,
        document: &Path,
        executable: &Path,
        reporting: Reporting,
    ) -> Result<Option<Self>, DocsightError> {
        let Ok(source) = DocumentSource::open(document) else {
            return Ok(None);
        };
        let cache = settings.open(executable)?;
        let key = cache.key_for(&source);
        let directory = tempfile::Builder::new()
            .prefix("docsight-cache-handoff-")
            .tempdir()
            .map_err(|source| DocsightError::Io {
                path: PathBuf::from("<cache-handoff>"),
                source,
            })?;
        let key_bytes = serde_json::to_vec(&key).map_err(output_serialization_error)?;
        write_new(&directory.path().join(HANDOFF_KEY_FILE), &key_bytes)?;
        let hit = match cache.get(&key)? {
            Lookup::Hit(document) => {
                let entry = encode_entry(&key, &document)?;
                write_new(&directory.path().join(HANDOFF_ENTRY_FILE), &entry)?;
                true
            }
            Lookup::Miss => false,
            Lookup::Quarantined(record) => {
                warn_quarantined(&record, reporting)?;
                false
            }
        };
        Ok(Some(Self {
            cache,
            key,
            executable: executable.to_path_buf(),
            directory,
            hit,
        }))
    }

    pub(crate) fn environment(&self) -> Result<Vec<(String, String)>, DocsightError> {
        let directory = self.directory.path();
        let mut read_paths = vec![path_text(&directory.join(HANDOFF_KEY_FILE))?];
        if self.hit {
            read_paths.push(path_text(&directory.join(HANDOFF_ENTRY_FILE))?);
        }
        let write_paths = vec![path_text(directory)?];
        Ok(vec![
            (CACHE_HANDOFF_ENV.to_owned(), path_text(directory)?),
            (
                docsight_worker::SANDBOX_READ_PATHS_ENV.to_owned(),
                serde_json::to_string(&read_paths).map_err(output_serialization_error)?,
            ),
            (
                docsight_worker::SANDBOX_WRITE_PATHS_ENV.to_owned(),
                serde_json::to_string(&write_paths).map_err(output_serialization_error)?,
            ),
        ])
    }

    pub(crate) fn commit(self, reporting: Reporting) -> Result<(), DocsightError> {
        if self.hit {
            return Ok(());
        }
        let result = self.directory.path().join(HANDOFF_RESULT_FILE);
        let limit = self.cache.max_bytes();
        let Some(bytes) = read_bounded(&result, limit)? else {
            return Ok(());
        };
        if bytes.len() as u64 > limit {
            return Ok(());
        }
        if engine_identity(&self.executable)? != *self.cache.engine() {
            return emit_warnings(
                &[Diagnostic::warning(
                    "CACHE_RESULT_DISCARDED",
                    format!(
                        "the sandbox worker executable {} changed while the document was parsed",
                        self.executable.display()
                    ),
                    "the parsed document IR was not stored in the cache",
                )],
                reporting.quiet,
                reporting.json_errors,
            );
        }
        match self.cache.accept_entry_bytes(&self.key, &bytes)? {
            Ok(_) => Ok(()),
            Err(record) => warn_quarantined(&record, reporting),
        }
    }
}

pub(crate) fn strip_cache_arguments(arguments: Vec<String>) -> Vec<String> {
    let mut stripped = Vec::with_capacity(arguments.len());
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            stripped.push(argument);
            stripped.extend(arguments);
            break;
        }
        if CACHE_FLAGS.contains(&argument.as_str()) {
            arguments.next();
            continue;
        }
        let inline = CACHE_FLAGS.iter().any(|flag| {
            argument
                .strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='))
        });
        if !inline {
            stripped.push(argument);
        }
    }
    stripped
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheAction {
    Stats,
    Verify,
    Prune,
    Clear,
}

impl CacheAction {
    fn name(self) -> &'static str {
        match self {
            Self::Stats => "stats",
            Self::Verify => "verify",
            Self::Prune => "prune",
            Self::Clear => "clear",
        }
    }
}

#[derive(Serialize)]
struct CacheCommandResult {
    action: &'static str,
    layout: &'static str,
    engine: EngineIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<CacheVerifyReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prune: Option<CachePruneReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clear: Option<CacheClearReport>,
    stats: CacheStats,
}

#[derive(Serialize)]
struct CacheEnvelope<'a> {
    schema: &'static str,
    engine: &'static str,
    result: &'a CacheCommandResult,
}

pub(crate) fn cache_command(
    settings: &CacheSettings,
    action: CacheAction,
    json: bool,
    ndjson: bool,
    reporting: Reporting,
) -> Result<(), DocsightError> {
    let cache = settings.open(&running_executable()?)?;
    let mut result = CacheCommandResult {
        action: action.name(),
        layout: CACHE_LAYOUT,
        engine: cache.engine().clone(),
        verify: None,
        prune: None,
        clear: None,
        stats: cache.stats()?,
    };
    let quarantined = match action {
        CacheAction::Stats => Vec::new(),
        CacheAction::Verify => {
            let report = cache.verify()?;
            let records = report.quarantined.clone();
            result.verify = Some(report);
            records
        }
        CacheAction::Prune => {
            let report = cache.prune()?;
            let records = report.quarantined.clone();
            result.prune = Some(report);
            records
        }
        CacheAction::Clear => {
            result.clear = Some(cache.clear()?);
            Vec::new()
        }
    };
    if action != CacheAction::Stats {
        result.stats = cache.stats()?;
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if ndjson {
        let meta = serde_json::json!({
            "seq": 1,
            "type": "meta",
            "schema": docsight_agent::AGENT_SCHEMA,
            "engine": env!("CARGO_PKG_VERSION"),
            "scope": "cache",
        });
        let mut item = serde_json::to_value(&result).map_err(output_serialization_error)?;
        if let Some(fields) = item.as_object_mut() {
            fields.insert("seq".to_owned(), serde_json::json!(2));
            fields.insert("type".to_owned(), serde_json::json!("cache"));
        }
        let done = serde_json::json!({
            "seq": 3,
            "type": "done",
            "limits": {
                "truncated": false,
                "total_items": 1,
                "returned_items": 1,
            },
        });
        for value in [meta, item, done] {
            serde_json::to_writer(&mut writer, &value).map_err(output_serialization_error)?;
            writer.write_all(b"\n").map_err(stdout_error)?;
        }
        return Ok(());
    }
    if json {
        let envelope = CacheEnvelope {
            schema: docsight_agent::AGENT_SCHEMA,
            engine: env!("CARGO_PKG_VERSION"),
            result: &result,
        };
        serde_json::to_writer(&mut writer, &envelope).map_err(output_serialization_error)?;
        return writer.write_all(b"\n").map_err(stdout_error);
    }

    let stats = &result.stats;
    let mut rows = vec![("action", result.action.to_owned())];
    if let Some(report) = &result.verify {
        rows.push(("checked", report.checked.to_string()));
        rows.push(("valid", report.valid.to_string()));
        rows.push(("other_engine", report.other_engine.to_string()));
        rows.push(("quarantined", report.quarantined.len().to_string()));
    }
    if let Some(report) = &result.prune {
        rows.push((
            "removed_quarantined_files",
            report.removed_quarantined_files.to_string(),
        ));
        rows.push((
            "removed_temporary_files",
            report.removed_temporary_files.to_string(),
        ));
        rows.push((
            "removed_other_engine",
            report.removed_other_engine_entries.to_string(),
        ));
        rows.push(("quarantined", report.quarantined.len().to_string()));
        rows.push(("evicted_entries", report.evicted_entries.to_string()));
    }
    if let Some(report) = &result.clear {
        rows.push(("removed_entries", report.removed_entries.to_string()));
        rows.push((
            "removed_quarantined_files",
            report.removed_quarantined_files.to_string(),
        ));
        rows.push((
            "removed_temporary_files",
            report.removed_temporary_files.to_string(),
        ));
    }
    rows.extend([
        ("entries", stats.entries.to_string()),
        ("entry_bytes", stats.entry_bytes.to_string()),
        (
            "current_engine_entries",
            stats.current_engine_entries.to_string(),
        ),
        (
            "other_engine_entries",
            stats.other_engine_entries.to_string(),
        ),
        ("unreadable_entries", stats.unreadable_entries.to_string()),
        ("quarantined_files", stats.quarantined_files.to_string()),
        ("quarantined_bytes", stats.quarantined_bytes.to_string()),
        ("temporary_files", stats.temporary_files.to_string()),
        ("max_bytes", stats.max_bytes.to_string()),
        ("max_entries", stats.max_entries.to_string()),
    ]);
    for (label, value) in rows {
        writeln!(writer, "{label:<26} {value}").map_err(stdout_error)?;
    }
    drop(writer);
    for record in &quarantined {
        warn_quarantined(record, reporting)?;
    }
    Ok(())
}

fn warn_quarantined(record: &QuarantineRecord, reporting: Reporting) -> Result<(), DocsightError> {
    emit_warnings(
        &[Diagnostic::warning(
            "CACHE_ENTRY_QUARANTINED",
            format!(
                "cache entry {} failed validation ({}) and was moved to quarantine",
                record.key_sha256,
                record.reason.code()
            ),
            "the entry was not used; the document IR is produced by parsing the document again",
        )],
        reporting.quiet,
        reporting.json_errors,
    )
}

fn engine_identity(executable: &Path) -> Result<EngineIdentity, DocsightError> {
    EngineIdentity::for_executable(
        executable,
        LAYOUT_PROFILE,
        &docsight_layout::font_fingerprint(),
    )
}

pub(crate) fn running_executable() -> Result<PathBuf, DocsightError> {
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/proc/self/exe"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::current_exe().map_err(|source| DocsightError::Io {
            path: PathBuf::from("<current-executable>"),
            source,
        })
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, DocsightError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DocsightError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(bytes))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), DocsightError> {
    let io_error = |source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = create_write_only(path).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)
}

fn create_write_only(path: &Path) -> io::Result<File> {
    File::options().write(true).create_new(true).open(path)
}

fn path_text(path: &Path) -> Result<String, DocsightError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        handoff_failure(format!(
            "cache handoff path {} is not valid UTF-8",
            path.display()
        ))
    })
}

fn handoff_failure(message: impl Into<String>) -> DocsightError {
    DocsightError::BackendFailure {
        backend: "docsight-cache".to_owned(),
        message: message.into(),
    }
}
