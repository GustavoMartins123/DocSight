use docsight_cache::{
    CacheConfig, CacheKey, DocumentCache, EngineIdentity, EntryLookup, EntryProducer,
    EntryRejection, Lookup, QuarantineReason, StoreOutcome, decode_entry, encode_entry,
};
use docsight_core::{Document, DocumentSource};
use docsight_ingest::ingest;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

type TestResult = Result<(), Box<dyn Error>>;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn engine(executable_marker: char) -> EngineIdentity {
    EngineIdentity {
        executable_sha256: executable_marker.to_string().repeat(64),
        engine_version: "0.0.0-test".to_owned(),
        ir_schema_version: docsight_core::IR_SCHEMA_VERSION.to_owned(),
        layout_profile: "agent-fidelity-v1".to_owned(),
        layout_font_fingerprint: "fonts-test".to_owned(),
    }
}

fn config(directory: &Path) -> CacheConfig {
    CacheConfig {
        directory: directory.to_path_buf(),
        max_bytes: docsight_cache::DEFAULT_CACHE_MAX_BYTES,
        max_entries: docsight_cache::DEFAULT_CACHE_MAX_ENTRIES,
    }
}

fn load(name: &str) -> Result<(DocumentSource, Document), Box<dyn Error>> {
    let source = DocumentSource::open(fixture(name))?;
    let document = ingest(&source)?;
    Ok((source, document))
}

fn entry_path(root: &Path, key: &CacheKey) -> Result<PathBuf, Box<dyn Error>> {
    Ok(root
        .join("v1")
        .join("entries")
        .join(format!("{}.dsc", key.digest()?)))
}

fn file_names(directory: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let mut names = Vec::new();
    for item in fs::read_dir(directory)? {
        names.push(item?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

fn expect_hit(lookup: Lookup) -> Result<Document, Box<dyn Error>> {
    match lookup {
        Lookup::Hit(document) => Ok(*document),
        other => Err(format!("expected cache hit, got {other:?}").into()),
    }
}

fn expect_quarantine(lookup: Lookup, reason: QuarantineReason) -> TestResult {
    match lookup {
        Lookup::Quarantined(record) if record.reason == reason => Ok(()),
        other => Err(format!("expected quarantine for {reason:?}, got {other:?}").into()),
    }
}

fn encode(key: &CacheKey, document: &Document) -> Result<Vec<u8>, Box<dyn Error>> {
    Ok(encode_entry(key, document, EntryProducer::InProcess)?)
}

fn raw_entry(key: &CacheKey, payload: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let header = serde_json::json!({
        "schema": "docsight.cache-entry/v1",
        "key": key,
        "producer": "in_process",
        "payload_sha256": digest,
        "payload_bytes": payload.len(),
    });
    let mut bytes = serde_json::to_vec(&header)?;
    bytes.push(b'\n');
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn store(
    cache: &DocumentCache,
    key: &CacheKey,
    document: &Document,
) -> Result<StoreOutcome, Box<dyn Error>> {
    Ok(cache.put(key, document, EntryProducer::InProcess)?)
}

fn set_age(path: &Path, age: Duration) -> TestResult {
    let file = fs::File::options().write(true).open(path)?;
    file.set_modified(SystemTime::now() - age)?;
    Ok(())
}

#[test]
fn miss_store_and_hit_return_the_identical_document() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let key = cache.key_for(&source);

    assert_eq!(cache.get(&key)?, Lookup::Miss);
    assert_eq!(store(&cache, &key, &document)?, StoreOutcome::Stored);
    assert_eq!(
        store(&cache, &key, &document)?,
        StoreOutcome::AlreadyPresent
    );

    let cached = expect_hit(cache.get(&key)?)?;
    assert_eq!(cached, document);
    assert_eq!(
        serde_json::to_vec(&cached)?,
        serde_json::to_vec(&document)?,
        "a cache hit must serialize byte-for-byte like the freshly ingested document"
    );
    assert!(file_names(&directory.path().join("v1").join("tmp"))?.is_empty());
    Ok(())
}

#[test]
fn entries_are_deterministic_for_the_same_key_and_document() -> TestResult {
    let (source, document) = load("sample_tables.docx")?;
    let key = CacheKey::document_ir(&source, &engine('a'));
    let first = encode(&key, &document)?;
    let second = encode(&key, &ingest(&source)?)?;
    assert_eq!(first, second);
    assert_eq!(decode_entry(&first, &key), Ok(document));
    Ok(())
}

#[test]
fn every_key_component_invalidates_the_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = engine('a');
    let (source, document) = load("sample_headings.docx")?;
    let cache = DocumentCache::open(&config(directory.path()), base.clone())?;
    store(&cache, &cache.key_for(&source), &document)?;

    let variants = [
        EngineIdentity {
            executable_sha256: "b".repeat(64),
            ..base.clone()
        },
        EngineIdentity {
            engine_version: "0.0.1-test".to_owned(),
            ..base.clone()
        },
        EngineIdentity {
            ir_schema_version: "0.0".to_owned(),
            ..base.clone()
        },
        EngineIdentity {
            layout_profile: "other-profile".to_owned(),
            ..base.clone()
        },
        EngineIdentity {
            layout_font_fingerprint: "other-fonts".to_owned(),
            ..base.clone()
        },
    ];
    for variant in variants {
        let other = DocumentCache::open(&config(directory.path()), variant)?;
        assert_eq!(other.get(&other.key_for(&source))?, Lookup::Miss);
    }

    let (other_source, _) = load("sample_tables.docx")?;
    assert_eq!(cache.get(&cache.key_for(&other_source))?, Lookup::Miss);
    expect_hit(cache.get(&cache.key_for(&source))?)?;
    Ok(())
}

#[test]
fn keys_from_another_engine_are_rejected_explicitly() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let foreign = CacheKey::document_ir(&source, &engine('b'));

    assert!(cache.get(&foreign).is_err());
    assert!(
        cache
            .put(&foreign, &document, EntryProducer::InProcess)
            .is_err()
    );
    Ok(())
}

#[test]
fn corrupted_entries_are_quarantined_and_never_returned() -> TestResult {
    let (source, document) = load("sample_headings.docx")?;
    let (other_source, other_document) = load("sample_tables.docx")?;
    let key = CacheKey::document_ir(&source, &engine('a'));
    let other_key = CacheKey::document_ir(&other_source, &engine('a'));
    let valid = encode(&key, &document)?;
    let newline = valid
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("entry header terminator")?;

    let mut flipped = valid.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x01;
    let mut truncated = valid.clone();
    truncated.truncate(valid.len() - 7);
    let mut bad_header = valid.clone();
    bad_header[1] = b'#';
    let mut wrong_identity = document.clone();
    wrong_identity.sha256 = other_document.sha256.clone();

    let cases: Vec<(Vec<u8>, EntryRejection)> = vec![
        (Vec::new(), EntryRejection::MissingHeader),
        (valid[..newline].to_vec(), EntryRejection::MissingHeader),
        (bad_header, EntryRejection::InvalidHeader),
        (
            encode(&other_key, &other_document)?,
            EntryRejection::KeyMismatch,
        ),
        (truncated, EntryRejection::PayloadLength),
        (flipped, EntryRejection::PayloadDigest),
        (
            raw_entry(&key, &serde_json::to_vec(&wrong_identity)?)?,
            EntryRejection::DocumentIdentity,
        ),
    ];

    for (bytes, rejection) in cases {
        let directory = tempfile::tempdir()?;
        let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
        let path = entry_path(directory.path(), &key)?;
        fs::write(&path, &bytes)?;

        expect_quarantine(cache.get(&key)?, QuarantineReason::Entry(rejection))?;
        assert!(
            !path.exists(),
            "{rejection:?} entry must leave the entries directory"
        );
        let quarantined = file_names(&directory.path().join("v1").join("quarantine"))?;
        assert_eq!(
            quarantined,
            vec![format!("{}.{}.dsc", key.digest()?, rejection.code())]
        );
        assert_eq!(cache.get(&key)?, Lookup::Miss);
        assert_eq!(store(&cache, &key, &document)?, StoreOutcome::Stored);
        assert_eq!(expect_hit(cache.get(&key)?)?, document);
    }
    Ok(())
}

#[test]
fn oversized_entries_are_quarantined_before_reading() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (source, document) = load("sample_headings.docx")?;
    let key = CacheKey::document_ir(&source, &engine('a'));
    let bytes = encode(&key, &document)?;
    let cache = DocumentCache::open(
        &CacheConfig {
            max_bytes: bytes.len() as u64 - 1,
            ..config(directory.path())
        },
        engine('a'),
    )?;
    fs::write(entry_path(directory.path(), &key)?, &bytes)?;

    expect_quarantine(cache.get(&key)?, QuarantineReason::Oversized)?;
    assert_eq!(store(&cache, &key, &document)?, StoreOutcome::ExceedsLimit);
    assert_eq!(cache.get(&key)?, Lookup::Miss);
    Ok(())
}

#[test]
fn interrupted_writes_do_not_create_entries_and_stale_temporaries_are_removed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let key = cache.key_for(&source);
    let bytes = encode(&key, &document)?;
    let temporary = directory.path().join("v1").join("tmp");
    let stale = temporary.join(".tmpstale");
    let fresh = temporary.join(".tmpfresh");
    fs::write(&stale, &bytes[..bytes.len() / 2])?;
    fs::write(&fresh, &bytes[..bytes.len() / 3])?;
    set_age(&stale, Duration::from_secs(3_600))?;

    assert_eq!(cache.get(&key)?, Lookup::Miss);
    assert_eq!(cache.stats()?.temporary_files, 2);
    store(&cache, &key, &document)?;
    assert_eq!(file_names(&temporary)?, vec![".tmpfresh".to_owned()]);
    expect_hit(cache.get(&key)?)?;
    Ok(())
}

#[test]
fn least_recently_used_entries_are_evicted_at_the_entry_limit() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(
        &CacheConfig {
            max_entries: 2,
            ..config(directory.path())
        },
        engine('a'),
    )?;
    let (first_source, first) = load("sample_headings.docx")?;
    let (second_source, second) = load("sample_tables.docx")?;
    let (third_source, third) = load("sample_features.docx")?;
    let first_key = cache.key_for(&first_source);
    let second_key = cache.key_for(&second_source);
    let third_key = cache.key_for(&third_source);

    store(&cache, &first_key, &first)?;
    store(&cache, &second_key, &second)?;
    set_age(
        &entry_path(directory.path(), &first_key)?,
        Duration::from_secs(200),
    )?;
    set_age(
        &entry_path(directory.path(), &second_key)?,
        Duration::from_secs(100),
    )?;
    expect_hit(cache.get(&first_key)?)?;
    store(&cache, &third_key, &third)?;

    assert_eq!(cache.stats()?.entries, 2);
    expect_hit(cache.get(&first_key)?)?;
    expect_hit(cache.get(&third_key)?)?;
    assert_eq!(cache.get(&second_key)?, Lookup::Miss);
    Ok(())
}

#[test]
fn byte_limit_is_enforced_at_the_exact_boundary() -> TestResult {
    let (first_source, first) = load("sample_headings.docx")?;
    let (second_source, second) = load("sample_tables.docx")?;
    let first_key = CacheKey::document_ir(&first_source, &engine('a'));
    let second_key = CacheKey::document_ir(&second_source, &engine('a'));
    let total = (encode(&first_key, &first)?.len() + encode(&second_key, &second)?.len()) as u64;

    for (max_bytes, expected_entries) in [(total, 2), (total - 1, 1)] {
        let directory = tempfile::tempdir()?;
        let cache = DocumentCache::open(
            &CacheConfig {
                max_bytes,
                ..config(directory.path())
            },
            engine('a'),
        )?;
        store(&cache, &first_key, &first)?;
        set_age(
            &entry_path(directory.path(), &first_key)?,
            Duration::from_secs(100),
        )?;
        store(&cache, &second_key, &second)?;
        let stats = cache.stats()?;
        assert_eq!(stats.entries, expected_entries, "max_bytes {max_bytes}");
        assert!(stats.entry_bytes <= max_bytes);
        expect_hit(cache.get(&second_key)?)?;
    }
    Ok(())
}

#[test]
fn zero_limits_are_rejected() -> TestResult {
    let directory = tempfile::tempdir()?;
    for limits in [(0, 1), (1, 0)] {
        let result = DocumentCache::open(
            &CacheConfig {
                directory: directory.path().to_path_buf(),
                max_bytes: limits.0,
                max_entries: limits.1,
            },
            engine('a'),
        );
        assert!(result.is_err());
    }
    Ok(())
}

#[test]
fn verify_prune_and_clear_report_the_cache_state() -> TestResult {
    let directory = tempfile::tempdir()?;
    let current = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let previous = DocumentCache::open(&config(directory.path()), engine('b'))?;
    let (first_source, first) = load("sample_headings.docx")?;
    let (second_source, second) = load("sample_tables.docx")?;
    let (third_source, third) = load("sample_features.docx")?;

    store(&current, &current.key_for(&first_source), &first)?;
    store(&previous, &previous.key_for(&first_source), &first)?;
    let renamed_key = current.key_for(&second_source);
    let misplaced_key = current.key_for(&third_source);
    fs::write(
        entry_path(directory.path(), &misplaced_key)?,
        encode(&renamed_key, &second)?,
    )?;
    let corrupt_key = CacheKey::document_ir(&third_source, &engine('c'));
    fs::write(entry_path(directory.path(), &corrupt_key)?, b"not an entry")?;
    fs::write(
        directory.path().join("v1").join("entries").join("README"),
        b"ignored",
    )?;
    let stale = directory.path().join("v1").join("tmp").join(".tmpold");
    fs::write(&stale, b"partial")?;
    set_age(&stale, Duration::from_secs(3_600))?;

    let stats = current.stats()?;
    assert_eq!(stats.entries, 4);
    assert_eq!(stats.current_engine_entries, 2);
    assert_eq!(stats.other_engine_entries, 1);
    assert_eq!(stats.unreadable_entries, 1);
    assert_eq!(stats.temporary_files, 1);

    let verify = current.verify()?;
    assert_eq!(verify.checked, 4);
    assert_eq!(verify.valid, 1);
    assert_eq!(verify.other_engine, 1);
    let mut reasons: Vec<QuarantineReason> = verify
        .quarantined
        .iter()
        .map(|record| record.reason)
        .collect();
    reasons.sort_by_key(|reason| format!("{reason:?}"));
    assert_eq!(
        reasons,
        vec![
            QuarantineReason::Entry(EntryRejection::MissingHeader),
            QuarantineReason::FileNameMismatch,
        ]
    );
    assert_eq!(current.stats()?.quarantined_files, 2);

    store(&current, &current.key_for(&third_source), &third)?;
    assert!(!stale.exists(), "store must remove stale temporary files");
    fs::write(&stale, b"partial")?;
    set_age(&stale, Duration::from_secs(3_600))?;
    let prune = current.prune()?;
    assert_eq!(prune.removed_quarantined_files, 2);
    assert_eq!(prune.removed_temporary_files, 1);
    assert_eq!(prune.removed_other_engine_entries, 1);
    assert!(prune.quarantined.is_empty());
    assert_eq!(prune.evicted_entries, 0);
    expect_hit(current.get(&current.key_for(&first_source))?)?;
    assert_eq!(
        previous.get(&previous.key_for(&first_source))?,
        Lookup::Miss
    );

    let clear = current.clear()?;
    assert_eq!(clear.removed_entries, 3);
    assert_eq!(clear.removed_quarantined_files, 0);
    let stats = current.stats()?;
    assert_eq!(stats.entries, 0);
    assert_eq!(stats.quarantined_files, 0);
    assert_eq!(stats.temporary_files, 0);
    assert!(directory.path().join("v1").join("CACHEDIR.TAG").is_file());
    Ok(())
}

#[test]
fn handed_off_entry_bytes_are_validated_before_commit() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let (other_source, other_document) = load("sample_tables.docx")?;
    let key = cache.key_for(&source);

    let forged = encode(&cache.key_for(&other_source), &other_document)?;
    match cache.accept_entry_bytes(&key, &forged)? {
        Err(record) => assert_eq!(
            record.reason,
            QuarantineReason::Entry(EntryRejection::KeyMismatch)
        ),
        Ok(outcome) => return Err(format!("forged entry was accepted: {outcome:?}").into()),
    }
    assert_eq!(cache.get(&key)?, Lookup::Miss);
    assert_eq!(cache.stats()?.quarantined_files, 1);

    let genuine = encode_entry(&key, &document, EntryProducer::SandboxWorker)?;
    assert_eq!(
        cache.accept_entry_bytes(&key, &genuine)?,
        Ok(StoreOutcome::Stored)
    );
    assert_eq!(
        fs::read(entry_path(directory.path(), &key)?)?,
        genuine,
        "the committed entry must be the bytes the parent validated"
    );
    assert_eq!(expect_hit(cache.get(&key)?)?, document);
    Ok(())
}

#[test]
fn concurrent_writers_and_readers_converge_on_one_valid_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (source, document) = load("sample_features.docx")?;
    let document = Arc::new(document);
    let root = directory.path().to_path_buf();
    let mut workers = Vec::new();
    for _ in 0..8 {
        let root = root.clone();
        let document = Arc::clone(&document);
        let source = source.clone();
        workers.push(std::thread::spawn(
            move || -> Result<(StoreOutcome, bool), String> {
                let cache = DocumentCache::open(&config(&root), engine('a'))
                    .map_err(|error| error.to_string())?;
                let key = cache.key_for(&source);
                let outcome = cache
                    .put(&key, &document, EntryProducer::InProcess)
                    .map_err(|error| error.to_string())?;
                let hit = match cache.get(&key).map_err(|error| error.to_string())? {
                    Lookup::Hit(cached) => *cached == *document,
                    _ => false,
                };
                Ok((outcome, hit))
            },
        ));
    }
    let mut stored = 0;
    for worker in workers {
        let (outcome, hit) = worker.join().map_err(|_| "worker panicked")??;
        assert!(hit);
        if outcome == StoreOutcome::Stored {
            stored += 1;
        }
    }
    assert_eq!(stored, 1);
    let cache = DocumentCache::open(&config(&root), engine('a'))?;
    let verify = cache.verify()?;
    assert_eq!((verify.checked, verify.valid), (1, 1));
    assert!(file_names(&root.join("v1").join("tmp"))?.is_empty());
    Ok(())
}

#[test]
fn engine_identity_hashes_the_executable_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("engine");
    fs::write(&executable, b"abc")?;
    let identity = EngineIdentity::for_executable(&executable, "profile", "fonts")?;
    assert_eq!(
        identity.executable_sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(identity.engine_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(identity.ir_schema_version, docsight_core::IR_SCHEMA_VERSION);
    fs::write(&executable, b"abd")?;
    assert_ne!(
        EngineIdentity::for_executable(&executable, "profile", "fonts")?,
        identity
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cache_layout_is_private_and_rejects_symbolic_links() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = tempfile::tempdir()?;
    let root = directory.path().join("cache");
    let cache = DocumentCache::open(&config(&root), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let key = cache.key_for(&source);
    store(&cache, &key, &document)?;

    for path in [
        root.clone(),
        root.join("v1"),
        root.join("v1").join("entries"),
        root.join("v1").join("tmp"),
        root.join("v1").join("quarantine"),
    ] {
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o700);
    }
    for path in [
        entry_path(&root, &key)?,
        root.join("v1").join("CACHEDIR.TAG"),
    ] {
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
    let tag = fs::read_to_string(root.join("v1").join("CACHEDIR.TAG"))?;
    assert!(tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55"));

    let target = directory.path().join("target.dsc");
    fs::rename(entry_path(&root, &key)?, &target)?;
    symlink(&target, entry_path(&root, &key)?)?;
    expect_quarantine(cache.get(&key)?, QuarantineReason::NotRegularFile)?;
    assert!(target.is_file());

    let linked = directory.path().join("linked");
    symlink(&root, &linked)?;
    assert!(DocumentCache::open(&config(&linked), engine('a')).is_err());
    let entries_link_root = directory.path().join("hijacked");
    fs::create_dir_all(entries_link_root.join("v1"))?;
    symlink(
        directory.path(),
        entries_link_root.join("v1").join("entries"),
    )?;
    assert!(DocumentCache::open(&config(&entries_link_root), engine('a')).is_err());
    Ok(())
}

#[test]
fn non_canonical_documents_are_rejected_before_they_reach_the_cache() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let key = cache.key_for(&source);
    let mut non_canonical = document.clone();
    non_canonical.blocks.reverse();

    assert!(encode_entry(&key, &non_canonical, EntryProducer::InProcess).is_err());
    assert!(
        cache
            .put(&key, &non_canonical, EntryProducer::InProcess)
            .is_err()
    );
    assert_eq!(cache.get(&key)?, Lookup::Miss);

    let forged = raw_entry(&key, &serde_json::to_vec(&non_canonical)?)?;
    match cache.accept_entry_bytes(&key, &forged)? {
        Err(record) => assert_eq!(
            record.reason,
            QuarantineReason::Entry(EntryRejection::NonCanonical)
        ),
        Ok(outcome) => return Err(format!("non-canonical handoff accepted: {outcome:?}").into()),
    }

    fs::write(entry_path(directory.path(), &key)?, &forged)?;
    let verify = cache.verify()?;
    assert_eq!(verify.valid, 0);
    assert_eq!(
        verify.quarantined.first().map(|record| record.reason),
        Some(QuarantineReason::Entry(EntryRejection::NonCanonical))
    );
    assert_eq!(cache.get(&key)?, Lookup::Miss);
    Ok(())
}

#[test]
fn entries_record_which_process_produced_them() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_headings.docx")?;
    let (other_source, other_document) = load("sample_tables.docx")?;
    let key = cache.key_for(&source);
    let other_key = cache.key_for(&other_source);

    store(&cache, &key, &document)?;
    let handed_off = encode_entry(&other_key, &other_document, EntryProducer::SandboxWorker)?;
    assert_eq!(
        cache.accept_entry_bytes(&other_key, &handed_off)?,
        Ok(StoreOutcome::Stored)
    );

    let stats = cache.stats()?;
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.in_process_entries, 1);
    assert_eq!(stats.sandbox_worker_entries, 1);
    assert_eq!(stats.current_engine_entries, 2);
    Ok(())
}

#[test]
fn verified_entry_bytes_are_returned_without_decoding() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache = DocumentCache::open(&config(directory.path()), engine('a'))?;
    let (source, document) = load("sample_tables.docx")?;
    let key = cache.key_for(&source);

    assert_eq!(cache.get_entry(&key)?, EntryLookup::Miss);
    store(&cache, &key, &document)?;
    assert_eq!(
        cache.get_entry(&key)?,
        EntryLookup::Hit(encode(&key, &document)?)
    );
    assert_eq!(cache.revalidate(&key)?, None);

    let mut corrupt = encode(&key, &document)?;
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0x04;
    fs::write(entry_path(directory.path(), &key)?, &corrupt)?;
    match cache.get_entry(&key)? {
        EntryLookup::Quarantined(record) => assert_eq!(
            record.reason,
            QuarantineReason::Entry(EntryRejection::PayloadDigest)
        ),
        other => return Err(format!("expected quarantine, got {other:?}").into()),
    }
    Ok(())
}
