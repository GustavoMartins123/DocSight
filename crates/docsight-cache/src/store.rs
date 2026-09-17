use crate::entry::{EntryRejection, decode_entry, encode_entry, verify_entry_integrity};
use crate::key::{CacheKey, EngineIdentity};
use docsight_core::{DocsightError, Document, DocumentSource};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const DEFAULT_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_CACHE_MAX_ENTRIES: u64 = 4_096;

const LAYOUT_DIRECTORY: &str = "v1";
const ENTRIES_DIRECTORY: &str = "entries";
const QUARANTINE_DIRECTORY: &str = "quarantine";
const TEMPORARY_DIRECTORY: &str = "tmp";
const ENTRY_EXTENSION: &str = "dsc";
const CACHEDIR_TAG_NAME: &str = "CACHEDIR.TAG";
const CACHEDIR_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# This directory holds DocSight document IR cache entries.\n";
const MAX_QUARANTINE_FILES: usize = 64;
const MAX_SCANNED_FILES: usize = 100_000;
const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(600);
const HEADER_PROBE_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheConfig {
    pub directory: PathBuf,
    pub max_bytes: u64,
    pub max_entries: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lookup {
    Hit(Box<Document>),
    Miss,
    Quarantined(QuarantineRecord),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreOutcome {
    Stored,
    AlreadyPresent,
    ExceedsLimit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QuarantineRecord {
    pub key_sha256: String,
    pub reason: QuarantineReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuarantineReason {
    NotRegularFile,
    Oversized,
    FileNameMismatch,
    Entry(EntryRejection),
}

impl Serialize for QuarantineReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl QuarantineReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotRegularFile => "not_regular_file",
            Self::Oversized => "oversized",
            Self::FileNameMismatch => "file_name_mismatch",
            Self::Entry(rejection) => rejection.code(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CacheStats {
    pub entries: u64,
    pub entry_bytes: u64,
    pub current_engine_entries: u64,
    pub other_engine_entries: u64,
    pub unreadable_entries: u64,
    pub quarantined_files: u64,
    pub quarantined_bytes: u64,
    pub temporary_files: u64,
    pub max_bytes: u64,
    pub max_entries: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CacheVerifyReport {
    pub checked: u64,
    pub valid: u64,
    pub other_engine: u64,
    pub quarantined: Vec<QuarantineRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CachePruneReport {
    pub removed_quarantined_files: u64,
    pub removed_temporary_files: u64,
    pub removed_other_engine_entries: u64,
    pub quarantined: Vec<QuarantineRecord>,
    pub evicted_entries: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CacheClearReport {
    pub removed_entries: u64,
    pub removed_quarantined_files: u64,
    pub removed_temporary_files: u64,
}

#[derive(Debug)]
pub struct DocumentCache {
    entries: PathBuf,
    quarantine: PathBuf,
    temporary: PathBuf,
    max_bytes: u64,
    max_entries: u64,
    engine: EngineIdentity,
}

enum Inspection {
    Current,
    OtherEngine,
    Quarantined(QuarantineRecord),
    Vanished,
}

struct ScannedFile {
    path: PathBuf,
    name: String,
    bytes: u64,
    modified: SystemTime,
    regular: bool,
}

impl DocumentCache {
    pub fn open(config: &CacheConfig, engine: EngineIdentity) -> Result<Self, DocsightError> {
        if config.max_bytes == 0 || config.max_entries == 0 {
            return Err(DocsightError::InvalidArgument {
                message: "cache limits must be greater than zero".to_owned(),
            });
        }
        ensure_directory(&config.directory)?;
        let root = config.directory.join(LAYOUT_DIRECTORY);
        ensure_directory(&root)?;
        let entries = root.join(ENTRIES_DIRECTORY);
        let quarantine = root.join(QUARANTINE_DIRECTORY);
        let temporary = root.join(TEMPORARY_DIRECTORY);
        for directory in [&entries, &quarantine, &temporary] {
            ensure_directory(directory)?;
        }
        write_tag(&root, &temporary)?;
        Ok(Self {
            entries,
            quarantine,
            temporary,
            max_bytes: config.max_bytes,
            max_entries: config.max_entries,
            engine,
        })
    }

    pub fn engine(&self) -> &EngineIdentity {
        &self.engine
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn key_for(&self, source: &DocumentSource) -> CacheKey {
        CacheKey::document_ir(source, &self.engine)
    }

    pub fn get(&self, key: &CacheKey) -> Result<Lookup, DocsightError> {
        self.require_engine(key)?;
        let digest = key.digest()?;
        let path = self.entry_path(&digest);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Lookup::Miss),
            Err(source) => return Err(DocsightError::Io { path, source }),
        };
        if !metadata.file_type().is_file() {
            return self
                .quarantine_file(&path, &digest, QuarantineReason::NotRegularFile)
                .map(Lookup::Quarantined);
        }
        if metadata.len() > self.max_bytes {
            return self
                .quarantine_file(&path, &digest, QuarantineReason::Oversized)
                .map(Lookup::Quarantined);
        }
        let bytes = match read_bounded(&path, self.max_bytes)? {
            Some(bytes) => bytes,
            None => return Ok(Lookup::Miss),
        };
        match decode_entry(&bytes, key) {
            Ok(document) => {
                touch(&path)?;
                Ok(Lookup::Hit(Box::new(document)))
            }
            Err(rejection) => self
                .quarantine_file(&path, &digest, QuarantineReason::Entry(rejection))
                .map(Lookup::Quarantined),
        }
    }

    pub fn put(&self, key: &CacheKey, document: &Document) -> Result<StoreOutcome, DocsightError> {
        self.require_engine(key)?;
        let bytes = encode_entry(key, document)?;
        if bytes.len() as u64 > self.max_bytes {
            return Ok(StoreOutcome::ExceedsLimit);
        }
        let digest = key.digest()?;
        let outcome = self.publish(&self.entry_path(&digest), &bytes)?;
        self.remove_stale_temporary_files(false)?;
        self.enforce_limits()?;
        Ok(outcome)
    }

    pub fn accept_entry_bytes(
        &self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> Result<Result<StoreOutcome, QuarantineRecord>, DocsightError> {
        self.require_engine(key)?;
        match decode_entry(bytes, key) {
            Ok(document) => self.put(key, &document).map(Ok),
            Err(rejection) => {
                let digest = key.digest()?;
                let reason = QuarantineReason::Entry(rejection);
                let name = quarantine_name(&digest, reason);
                self.publish_replacing(&self.quarantine.join(name), bytes)?;
                self.trim_quarantine()?;
                Ok(Err(QuarantineRecord {
                    key_sha256: digest,
                    reason,
                }))
            }
        }
    }

    pub fn stats(&self) -> Result<CacheStats, DocsightError> {
        let entries = scan(&self.entries)?;
        let quarantine = scan(&self.quarantine)?;
        let temporary = scan(&self.temporary)?;
        let mut stats = CacheStats {
            entries: 0,
            entry_bytes: 0,
            current_engine_entries: 0,
            other_engine_entries: 0,
            unreadable_entries: 0,
            quarantined_files: quarantine.len() as u64,
            quarantined_bytes: quarantine.iter().map(|file| file.bytes).sum(),
            temporary_files: temporary.len() as u64,
            max_bytes: self.max_bytes,
            max_entries: self.max_entries,
        };
        for file in entries.iter().filter(|file| is_entry_name(&file.name)) {
            stats.entries += 1;
            stats.entry_bytes += file.bytes;
            match file.regular.then(|| probe_engine(&file.path)).flatten() {
                Some(engine) if engine == self.engine => stats.current_engine_entries += 1,
                Some(_) => stats.other_engine_entries += 1,
                None => stats.unreadable_entries += 1,
            }
        }
        Ok(stats)
    }

    pub fn verify(&self) -> Result<CacheVerifyReport, DocsightError> {
        let mut report = CacheVerifyReport {
            checked: 0,
            valid: 0,
            other_engine: 0,
            quarantined: Vec::new(),
        };
        for file in scan(&self.entries)?
            .into_iter()
            .filter(|file| is_entry_name(&file.name))
        {
            match self.inspect_file(&file)? {
                Inspection::Current => report.valid += 1,
                Inspection::OtherEngine => report.other_engine += 1,
                Inspection::Quarantined(record) => report.quarantined.push(record),
                Inspection::Vanished => continue,
            }
            report.checked += 1;
        }
        Ok(report)
    }

    pub fn prune(&self) -> Result<CachePruneReport, DocsightError> {
        let removed_quarantined_files = remove_files(&self.quarantine)?;
        let removed_temporary_files = self.remove_stale_temporary_files(false)?;
        let mut removed_other_engine_entries = 0;
        let mut quarantined = Vec::new();
        for file in scan(&self.entries)?
            .into_iter()
            .filter(|file| is_entry_name(&file.name))
        {
            match self.inspect_file(&file)? {
                Inspection::Current | Inspection::Vanished => {}
                Inspection::OtherEngine => {
                    if remove_if_present(&file.path)? {
                        removed_other_engine_entries += 1;
                    }
                }
                Inspection::Quarantined(record) => quarantined.push(record),
            }
        }
        let evicted_entries = self.enforce_limits()?;
        Ok(CachePruneReport {
            removed_quarantined_files,
            removed_temporary_files,
            removed_other_engine_entries,
            quarantined,
            evicted_entries,
        })
    }

    pub fn clear(&self) -> Result<CacheClearReport, DocsightError> {
        Ok(CacheClearReport {
            removed_entries: remove_files(&self.entries)?,
            removed_quarantined_files: remove_files(&self.quarantine)?,
            removed_temporary_files: self.remove_stale_temporary_files(true)?,
        })
    }

    fn require_engine(&self, key: &CacheKey) -> Result<(), DocsightError> {
        if key.engine == self.engine && key.is_well_formed() {
            return Ok(());
        }
        Err(DocsightError::InvalidArgument {
            message: "cache key does not belong to the engine that opened the cache".to_owned(),
        })
    }

    fn entry_path(&self, digest: &str) -> PathBuf {
        self.entries.join(format!("{digest}.{ENTRY_EXTENSION}"))
    }

    fn inspect_file(&self, file: &ScannedFile) -> Result<Inspection, DocsightError> {
        let digest = file
            .name
            .strip_suffix(&format!(".{ENTRY_EXTENSION}"))
            .unwrap_or(&file.name)
            .to_owned();
        if !file.regular {
            return self
                .quarantine_file(&file.path, &digest, QuarantineReason::NotRegularFile)
                .map(Inspection::Quarantined);
        }
        if file.bytes > self.max_bytes {
            return self
                .quarantine_file(&file.path, &digest, QuarantineReason::Oversized)
                .map(Inspection::Quarantined);
        }
        let Some(bytes) = read_bounded(&file.path, self.max_bytes)? else {
            return Ok(Inspection::Vanished);
        };
        let rejection = match verify_entry_integrity(&bytes) {
            Ok(key) if key.digest()? != digest => QuarantineReason::FileNameMismatch,
            Ok(key) if key.engine != self.engine => return Ok(Inspection::OtherEngine),
            Ok(key) => match decode_entry(&bytes, &key) {
                Ok(_) => return Ok(Inspection::Current),
                Err(rejection) => QuarantineReason::Entry(rejection),
            },
            Err(rejection) => QuarantineReason::Entry(rejection),
        };
        self.quarantine_file(&file.path, &digest, rejection)
            .map(Inspection::Quarantined)
    }

    fn quarantine_file(
        &self,
        path: &Path,
        digest: &str,
        reason: QuarantineReason,
    ) -> Result<QuarantineRecord, DocsightError> {
        let target = self.quarantine.join(quarantine_name(digest, reason));
        match fs::rename(path, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(DocsightError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
        self.trim_quarantine()?;
        Ok(QuarantineRecord {
            key_sha256: digest.to_owned(),
            reason,
        })
    }

    fn trim_quarantine(&self) -> Result<(), DocsightError> {
        let mut files = scan(&self.quarantine)?;
        if files.len() <= MAX_QUARANTINE_FILES {
            return Ok(());
        }
        sort_oldest_first(&mut files);
        let excess = files.len() - MAX_QUARANTINE_FILES;
        for file in files.into_iter().take(excess) {
            remove_if_present(&file.path)?;
        }
        Ok(())
    }

    fn publish(&self, target: &Path, bytes: &[u8]) -> Result<StoreOutcome, DocsightError> {
        let temporary = self.write_temporary(bytes)?;
        match temporary.persist_noclobber(target) {
            Ok(_) => Ok(StoreOutcome::Stored),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(StoreOutcome::AlreadyPresent)
            }
            Err(error) => Err(DocsightError::Io {
                path: target.to_path_buf(),
                source: error.error,
            }),
        }
    }

    fn publish_replacing(&self, target: &Path, bytes: &[u8]) -> Result<(), DocsightError> {
        let temporary = self.write_temporary(bytes)?;
        temporary
            .persist(target)
            .map(|_| ())
            .map_err(|error| DocsightError::Io {
                path: target.to_path_buf(),
                source: error.error,
            })
    }

    fn write_temporary(&self, bytes: &[u8]) -> Result<tempfile::NamedTempFile, DocsightError> {
        let io_error = |source| DocsightError::Io {
            path: self.temporary.clone(),
            source,
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&self.temporary).map_err(io_error)?;
        temporary.write_all(bytes).map_err(io_error)?;
        temporary.as_file().sync_all().map_err(io_error)?;
        Ok(temporary)
    }

    fn remove_stale_temporary_files(&self, all: bool) -> Result<u64, DocsightError> {
        let now = SystemTime::now();
        let mut removed = 0;
        for file in scan(&self.temporary)? {
            let stale = now
                .duration_since(file.modified)
                .is_ok_and(|age| age >= STALE_TEMPORARY_AGE);
            if (all || stale) && remove_if_present(&file.path)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn enforce_limits(&self) -> Result<u64, DocsightError> {
        let mut files: Vec<ScannedFile> = scan(&self.entries)?
            .into_iter()
            .filter(|file| is_entry_name(&file.name))
            .collect();
        sort_oldest_first(&mut files);
        let mut total_bytes: u64 = files.iter().map(|file| file.bytes).sum();
        let mut total_entries = files.len() as u64;
        let mut evicted = 0;
        for file in files {
            if total_bytes <= self.max_bytes && total_entries <= self.max_entries {
                break;
            }
            if remove_if_present(&file.path)? {
                evicted += 1;
            }
            total_bytes = total_bytes.saturating_sub(file.bytes);
            total_entries = total_entries.saturating_sub(1);
        }
        Ok(evicted)
    }
}

fn ensure_directory(path: &Path) -> Result<(), DocsightError> {
    let io_error = |source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    };
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(DocsightError::InvalidArgument {
            message: format!(
                "cache path {} must be a directory and not a symbolic link",
                path.display()
            ),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(io_error)?;
            ensure_directory(path)
        }
        Err(source) => Err(io_error(source)),
    }
}

fn write_tag(root: &Path, temporary: &Path) -> Result<(), DocsightError> {
    let target = root.join(CACHEDIR_TAG_NAME);
    if fs::symlink_metadata(&target).is_ok() {
        return Ok(());
    }
    let io_error = |source| DocsightError::Io {
        path: target.clone(),
        source,
    };
    let mut file = tempfile::NamedTempFile::new_in(temporary).map_err(io_error)?;
    file.write_all(CACHEDIR_TAG.as_bytes()).map_err(io_error)?;
    match file.persist_noclobber(&target) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(io_error(error.error)),
    }
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, DocsightError> {
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
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| DocsightError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(bytes))
}

fn probe_engine(path: &Path) -> Option<EngineIdentity> {
    #[derive(serde::Deserialize)]
    struct ProbeHeader {
        key: CacheKey,
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(HEADER_PROBE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let newline = bytes.iter().position(|byte| *byte == b'\n')?;
    serde_json::from_slice::<ProbeHeader>(&bytes[..newline])
        .ok()
        .map(|header| header.key.engine)
}

fn touch(path: &Path) -> Result<(), DocsightError> {
    let io_error = |source| DocsightError::Io {
        path: path.to_path_buf(),
        source,
    };
    match File::options().write(true).open(path) {
        Ok(file) => file.set_modified(SystemTime::now()).map_err(io_error),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(source)),
    }
}

fn scan(directory: &Path) -> Result<Vec<ScannedFile>, DocsightError> {
    let io_error = |source| DocsightError::Io {
        path: directory.to_path_buf(),
        source,
    };
    let mut files = Vec::new();
    for item in fs::read_dir(directory).map_err(io_error)? {
        let item = item.map_err(io_error)?;
        if files.len() >= MAX_SCANNED_FILES {
            return Err(DocsightError::ResourceLimit {
                resource: "cache directory files".to_owned(),
                limit: MAX_SCANNED_FILES as u64,
            });
        }
        let file_type = item.file_type().map_err(io_error)?;
        let metadata = match fs::symlink_metadata(item.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(source)),
        };
        files.push(ScannedFile {
            path: item.path(),
            name: item.file_name().to_string_lossy().into_owned(),
            bytes: metadata.len(),
            modified: metadata.modified().map_err(io_error)?,
            regular: file_type.is_file(),
        });
    }
    files.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(files)
}

fn sort_oldest_first(files: &mut [ScannedFile]) {
    files.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn remove_files(directory: &Path) -> Result<u64, DocsightError> {
    let mut removed = 0;
    for file in scan(directory)? {
        if remove_if_present(&file.path)? {
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_if_present(path: &Path) -> Result<bool, DocsightError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(DocsightError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn is_entry_name(name: &str) -> bool {
    name.strip_suffix(&format!(".{ENTRY_EXTENSION}"))
        .is_some_and(crate::key::is_sha256)
}

fn quarantine_name(digest: &str, reason: QuarantineReason) -> String {
    format!("{digest}.{}.{ENTRY_EXTENSION}", reason.code())
}
