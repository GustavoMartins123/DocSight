use super::distribution::{SignatureReceipt, load_policy, validate_signature_receipt};
use super::{
    DOCUMENTS, EXAMPLES, SmokeReceipt, TARGETS, basename, binary, binary_names,
    validate_smoke_receipt,
};
use crate::tooling::common::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const MAX_TOTAL_BYTES: u64 = 536_870_912;
pub const SCHEMA: &str = "docsight.release/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub version: String,
    pub target: String,
    pub revision: String,
    pub toolchain: String,
    pub files: Vec<Record>,
}

fn corrupt<E>(_: E) -> ToolError {
    ToolError::new(
        "CORRUPT_ARCHIVE",
        "Archive is corrupt or missing declared content",
    )
}

fn options(executable: bool) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .unix_permissions(if executable { 0o755 } else { 0o644 })
}

pub fn required_files(target: &str) -> Result<BTreeSet<String>> {
    let (binary, worker) = binary_names(target)?;
    let mut names: BTreeSet<String> = DOCUMENTS.into_iter().map(str::to_owned).collect();
    names.extend(
        [
            binary,
            worker,
            "THIRD_PARTY_NOTICES.md",
            "schemas/v2/agent-envelope.json",
            "schemas/ir/v1/document-ir.json",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    names.extend(EXAMPLES.into_iter().map(|name| format!("examples/{name}")));
    Ok(names)
}

fn toolchain(root: &Path) -> Result<String> {
    let data = read_bytes(&root.join("rust-toolchain.toml"), 65_536)?;
    let channels: Vec<_> = text(&data)?
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| key.trim() == "channel")
        .collect();
    require(
        channels.len() == 1,
        "INVALID_TOOLCHAIN",
        "Exactly one pinned Rust toolchain is required",
    )?;
    let value = parse_json(channels[0].1.trim().as_bytes())?;
    let channel = string(&value)?;
    checked_version(channel)?;
    require(
        !channel.contains('-'),
        "INVALID_TOOLCHAIN",
        "Toolchain must be a pinned stable Rust release",
    )?;
    Ok(channel.to_owned())
}

pub fn make_package(
    root: &Path,
    binary_path: &Path,
    worker_path: &Path,
    target: &str,
    revision: &str,
    notices: &Path,
    destination: &Path,
) -> Result<PathBuf> {
    checked_revision(revision)?;
    let version = workspace_version();
    let base = basename(version, target)?;
    let (binary_name, worker_name) = binary_names(target)?;
    let mut payload: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in DOCUMENTS {
        payload.insert(name.to_owned(), contained_file(root, name)?);
    }
    payload.insert(binary_name.into(), binary_path.to_path_buf());
    payload.insert(worker_name.into(), worker_path.to_path_buf());
    payload.insert("THIRD_PARTY_NOTICES.md".into(), notices.to_path_buf());
    for example in EXAMPLES {
        payload.insert(
            format!("examples/{example}"),
            contained_file(root, &format!("fixtures/validation/{example}"))?,
        );
    }
    for schema in list_files(&root.join("schemas"), 2048)? {
        if schema.extension().is_some_and(|value| value == "json") {
            let name = schema
                .strip_prefix(root)
                .map_err(corrupt)?
                .to_string_lossy()
                .replace('\\', "/");
            safe_member(&name)?;
            payload.insert(name, schema);
        }
    }
    require(
        required_files(target)?
            .iter()
            .all(|name| payload.contains_key(name)),
        "MISSING_SCHEMAS",
        "Required release resources are missing",
    )?;
    require(
        payload.len() < 256,
        "RESOURCE_LIMIT",
        "Release contains too many files",
    )?;
    fs::create_dir_all(destination)?;
    no_symlinks(destination)?;
    let final_path = destination.join(format!("{base}.zip"));
    let checksum = destination.join(format!("{base}.zip.sha256"));
    require(
        !final_path.try_exists()? && !checksum.try_exists()?,
        "OUTPUT_EXISTS",
        "Release output already exists",
    )?;
    let stage = tempfile::tempdir_in(destination)?;
    let staged_path = stage.path().join(format!("{base}.zip"));
    let mut writer = ZipWriter::new(File::create(&staged_path)?);
    let mut files = Vec::new();
    let mut total = 0u64;
    for (name, path) in payload {
        safe_member(&name)?;
        no_symlinks(&path)?;
        let bytes = read_bytes(&path, MAX_FILE_BYTES)?;
        require(
            !bytes.is_empty(),
            "MISSING_RELEASE_FILE",
            "Release resources must not be empty",
        )?;
        let size = u64::try_from(bytes.len()).map_err(corrupt)?;
        total = total.checked_add(size).ok_or_else(|| corrupt(()))?;
        require(
            total <= MAX_TOTAL_BYTES - MAX_JSON_BYTES,
            "RELEASE_SIZE_LIMIT",
            "Expanded release exceeds its byte limit",
        )?;
        let executable = name == binary_name || name == worker_name;
        if executable {
            binary::check_bytes(&bytes, target)?;
        }
        files.push(Record {
            path: name.clone(),
            size,
            sha256: digest(&bytes),
            executable,
        });
        writer
            .start_file(format!("{base}/{name}"), options(executable))
            .map_err(corrupt)?;
        writer.write_all(&bytes)?;
    }
    let manifest = Manifest {
        schema: SCHEMA.into(),
        version: version.into(),
        target: target.into(),
        revision: revision.into(),
        toolchain: toolchain(root)?,
        files,
    };
    writer
        .start_file(format!("{base}/release-manifest.json"), options(false))
        .map_err(corrupt)?;
    writer.write_all(&json_bytes(&manifest)?)?;
    writer.finish().map_err(corrupt)?.sync_all()?;
    verify_archive(&staged_path)?;
    let hash = sha256_file(&staged_path)?;
    fs::hard_link(&staged_path, &final_path)?;
    if let Err(error) = write_new(&checksum, format!("{hash}  {base}.zip\n").as_bytes(), false) {
        fs::remove_file(&final_path)?;
        return Err(error);
    }
    Ok(final_path)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    archive
        .by_name(name)
        .map_err(corrupt)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(corrupt)?;
    require(
        u64::try_from(bytes.len()).is_ok_and(|size| size <= maximum),
        "RELEASE_SIZE_LIMIT",
        "Expanded member exceeds its byte limit",
    )?;
    Ok(bytes)
}

pub fn verify_archive(path: &Path) -> Result<Manifest> {
    no_symlinks(path)?;
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.is_file() && (1..=MAX_TOTAL_BYTES).contains(&metadata.len()),
        "INVALID_ARCHIVE",
        "Archive must be a bounded regular file",
    )?;
    let mut archive = ZipArchive::new(File::open(path)?).map_err(corrupt)?;
    require(
        (1..=256).contains(&archive.len()),
        "INVALID_ARCHIVE",
        "Archive member count is outside its bounds",
    )?;
    let mut members = BTreeMap::new();
    let mut folded = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(corrupt)?;
        let name = safe_member(entry.name())?.to_owned();
        let mode = entry.unix_mode().ok_or_else(|| corrupt(()))?;
        require(
            folded.insert(name.to_ascii_lowercase()) && mode & 0o170000 == 0o100000,
            "INVALID_ARCHIVE_MEMBER",
            "Archive contains duplicate or special members",
        )?;
        require(
            (1..=MAX_FILE_BYTES).contains(&entry.size()),
            "RELEASE_SIZE_LIMIT",
            "Archive member size is outside its bounds",
        )?;
        total = total.checked_add(entry.size()).ok_or_else(|| corrupt(()))?;
        require(
            total <= MAX_TOTAL_BYTES,
            "RELEASE_SIZE_LIMIT",
            "Expanded archive exceeds its byte limit",
        )?;
        members.insert(name, (entry.size(), mode));
    }
    let manifests: Vec<_> = members
        .keys()
        .filter(|name| name.ends_with("/release-manifest.json"))
        .cloned()
        .collect();
    require(
        manifests.len() == 1,
        "INVALID_MANIFEST",
        "Exactly one release manifest is required",
    )?;
    let manifest: Manifest = decode(parse_json(&read_entry(
        &mut archive,
        &manifests[0],
        MAX_JSON_BYTES,
    )?)?)?;
    require(
        manifest.schema == SCHEMA,
        "INVALID_MANIFEST_SCHEMA",
        "Release manifest schema is unsupported",
    )?;
    checked_revision(&manifest.revision)?;
    checked_version(&manifest.toolchain)?;
    require(
        !manifest.toolchain.contains('-'),
        "INVALID_TOOLCHAIN",
        "Release requires a pinned stable Rust toolchain",
    )?;
    let base = basename(&manifest.version, &manifest.target)?;
    require(
        manifests[0] == format!("{base}/release-manifest.json")
            && path
                .file_name()
                .is_some_and(|name| name == format!("{base}.zip").as_str()),
        "ARCHIVE_IDENTITY_MISMATCH",
        "Archive name, root and manifest identity must agree",
    )?;
    let (binary_name, worker_name) = binary_names(&manifest.target)?;
    let mut expected = BTreeSet::from([manifests[0].clone()]);
    let mut previous: Option<&str> = None;
    for record in &manifest.files {
        safe_member(&record.path)?;
        checked_digest(&record.sha256)?;
        require(
            previous.is_none_or(|name| name < record.path.as_str()),
            "RELEASE_CONTENT_MISMATCH",
            "Manifest paths must be sorted and unique",
        )?;
        previous = Some(&record.path);
        let full = format!("{base}/{}", record.path);
        require(
            expected.insert(full.clone()),
            "INVALID_MANIFEST",
            "Manifest path is duplicated",
        )?;
        let executable = record.path == binary_name || record.path == worker_name;
        require(
            record.executable == executable,
            "INVALID_EXECUTABLE_MODE",
            "Only the two native binaries may be executable",
        )?;
        let mode = if executable { 0o100755 } else { 0o100644 };
        require(
            members.get(&full) == Some(&(record.size, mode)),
            "RELEASE_METADATA_MISMATCH",
            "File size or permission differs from manifest",
        )?;
        let bytes = read_entry(&mut archive, &full, record.size.min(MAX_FILE_BYTES))?;
        require(
            u64::try_from(bytes.len()).ok() == Some(record.size) && digest(&bytes) == record.sha256,
            "RELEASE_DIGEST_MISMATCH",
            "Release content differs from its recorded digest",
        )?;
        if executable {
            binary::check_bytes(&bytes, &manifest.target)?;
        }
    }
    require(
        expected == members.keys().cloned().collect(),
        "RELEASE_CONTENT_MISMATCH",
        "Archive contains missing or extra members",
    )?;
    let declared: BTreeSet<_> = manifest
        .files
        .iter()
        .map(|record| record.path.clone())
        .collect();
    require(
        required_files(&manifest.target)?.is_subset(&declared),
        "INCOMPLETE_RELEASE",
        "Archive lacks required binaries, documentation, licenses, examples or schemas",
    )?;
    Ok(manifest)
}

pub fn extract_verified(path: &Path, destination: &Path) -> Result<(PathBuf, Manifest)> {
    let before = sha256_file(path)?;
    let manifest = verify_archive(path)?;
    let base = basename(&manifest.version, &manifest.target)?;
    fs::create_dir(destination)?;
    no_symlinks(destination)?;
    let result = (|| {
        let mut archive = ZipArchive::new(File::open(path)?).map_err(corrupt)?;
        for record in &manifest.files {
            let bytes = read_entry(
                &mut archive,
                &format!("{base}/{}", record.path),
                record.size,
            )?;
            require(
                u64::try_from(bytes.len()).ok() == Some(record.size)
                    && digest(&bytes) == record.sha256,
                "ARCHIVE_CHANGED",
                "Archive changed after verification",
            )?;
            let output = destination.join(&record.path);
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            write_new(&output, &bytes, record.executable)?;
        }
        let manifest_bytes = read_entry(
            &mut archive,
            &format!("{base}/release-manifest.json"),
            MAX_JSON_BYTES,
        )?;
        let extracted_manifest: Manifest = decode(parse_json(&manifest_bytes)?)?;
        require(
            extracted_manifest == manifest,
            "ARCHIVE_CHANGED",
            "Manifest changed after verification",
        )?;
        write_new(
            &destination.join("release-manifest.json"),
            &manifest_bytes,
            false,
        )?;
        require(
            sha256_file(path)? == before,
            "ARCHIVE_CHANGED",
            "Archive changed during extraction",
        )?;
        let (binary, worker) = binary_names(&manifest.target)?;
        binary::check_binary(&destination.join(binary), &manifest.target)?;
        binary::check_binary(&destination.join(worker), &manifest.target)?;
        Ok((destination.join(binary), manifest))
    })();
    if result.is_err() {
        fs::remove_dir_all(destination)?;
    }
    result
}

pub fn packaged_executables(path: &Path, manifest: &Manifest) -> Result<Vec<(String, Vec<u8>)>> {
    let base = basename(&manifest.version, &manifest.target)?;
    let (binary, worker) = binary_names(&manifest.target)?;
    let mut archive = ZipArchive::new(File::open(path)?).map_err(corrupt)?;
    let mut executables = Vec::new();
    for name in [binary, worker] {
        let record = manifest
            .files
            .iter()
            .find(|record| record.path == name && record.executable)
            .ok_or_else(|| corrupt(()))?;
        let bytes = read_entry(&mut archive, &format!("{base}/{name}"), record.size)?;
        require(
            digest(&bytes) == record.sha256,
            "ARCHIVE_CHANGED",
            "Archive changed after verification",
        )?;
        executables.push((name.to_owned(), bytes));
    }
    Ok(executables)
}

pub fn verify_sidecar(path: &Path) -> Result<String> {
    let hash = sha256_file(path)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| corrupt(()))?;
    let expected = format!("{hash}  {name}\n");
    let sidecar = path.with_file_name(format!("{name}.sha256"));
    require(
        read_bytes(&sidecar, 1024)? == expected.as_bytes(),
        "ARCHIVE_CHECKSUM_MISMATCH",
        "Archive checksum differs from its sidecar",
    )?;
    Ok(hash)
}

pub fn collect(directory: &Path, root: &Path) -> Result<Value> {
    let policy = load_policy(root)?;
    let paths = flat_files(directory, "zip", 10_000)?;
    require(
        paths.len() == TARGETS.len(),
        "INCOMPLETE_RELEASE_SET",
        "Exactly five platform archives are required",
    )?;
    let mut targets = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut signatures = BTreeMap::new();
    let mut sums = String::new();
    for path in paths {
        let manifest = verify_archive(&path)?;
        let hash = verify_sidecar(&path)?;
        let receipt: SmokeReceipt = decode(read_json(
            &directory.join(format!("smoke-{}.json", manifest.target)),
        )?)?;
        validate_smoke_receipt(&receipt, &manifest, &hash)?;
        let signature: SignatureReceipt = decode(read_json(
            &directory.join(format!("signature-{}.json", manifest.target)),
        )?)?;
        validate_signature_receipt(&signature, &path, &manifest, &hash, &policy)?;
        signatures.insert(manifest.target.clone(), signature.status);
        targets.insert(manifest.target.clone());
        identities.insert((manifest.version, manifest.revision, manifest.toolchain));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| corrupt(()))?;
        sums.push_str(&format!("{hash}  {name}\n"));
    }
    require(
        targets == TARGETS.into_iter().map(str::to_owned).collect() && identities.len() == 1,
        "INCONSISTENT_RELEASE_SET",
        "Targets must describe the same version, revision and toolchain",
    )?;
    let (version, revision, toolchain) =
        identities.into_iter().next().ok_or_else(|| corrupt(()))?;
    write_new(&directory.join("SHA256SUMS"), sums.as_bytes(), false)?;
    Ok(json!({
        "version": version,
        "revision": revision,
        "toolchain": toolchain,
        "targets": targets,
        "distribution_policy_sha256": policy.sha256,
        "provenance": policy.policy.provenance.status,
        "signatures": signatures,
    }))
}
