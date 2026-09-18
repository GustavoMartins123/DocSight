#[allow(dead_code)]
mod support;

use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::release::archive::{
    collect, extract_verified, make_package, required_files, verify_archive, verify_sidecar,
};
use xtask::release::binary::check_bytes;
use xtask::release::{SmokeReceipt, TARGETS, basename, matrix, validate_smoke_receipt};
use xtask::tooling::common::{sha256_file, workspace_root, workspace_version};

const LINUX: &str = "x86_64-unknown-linux-gnu";

type ArchiveEdit = Box<dyn Fn(&mut Vec<Entry>, &str)>;

fn base(target: &str) -> TestResult<String> {
    Ok(basename(workspace_version(), target)?)
}

fn member(target: &str, name: &str) -> TestResult<String> {
    Ok(format!("{}/{name}", base(target)?))
}

fn package_with(fixture: &Fixture, binary: &[u8], target: &str) -> TestResult<PathBuf> {
    let path = fixture.root.join("custom-binary");
    fs::write(&path, binary)?;
    Ok(make_package(
        &fixture.root,
        &path,
        &path,
        target,
        REVISION,
        &fixture.root.join("NOTICES"),
        &fixture.root.join("custom"),
    )?)
}

#[test]
fn every_target_packages_and_verifies_with_both_native_binaries() -> TestResult {
    let fixture = Fixture::new()?;
    for target in TARGETS {
        let archive = fixture.package(target, &format!("dist-{target}"))?;
        let manifest = verify_archive(&archive)?;
        assert_eq!(manifest.target, target);
        assert_eq!(manifest.revision, REVISION);
        assert_eq!(manifest.version, workspace_version());
        assert_eq!(manifest.toolchain, "1.96.0");
        let declared: BTreeSet<_> = manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        assert!(required_files(target)?.is_subset(&declared));
        let executables: BTreeSet<_> = manifest
            .files
            .iter()
            .filter(|file| file.executable)
            .map(|file| file.path.as_str())
            .collect();
        let expected: BTreeSet<_> = if target.contains("windows") {
            ["docsight-worker.exe", "docsight.exe"].into()
        } else {
            ["docsight", "docsight-worker"].into()
        };
        assert_eq!(executables, expected);
        assert_eq!(verify_sidecar(&archive)?, sha256_file(&archive)?);
    }
    Ok(())
}

#[test]
fn identical_inputs_produce_identical_archives() -> TestResult {
    let fixture = Fixture::new()?;
    let one = fixture.package(LINUX, "one")?;
    let two = fixture.package(LINUX, "two")?;
    assert_eq!(fs::read(&one)?, fs::read(&two)?);
    assert_eq!(
        fs::read(one.with_extension("zip.sha256"))?,
        fs::read(two.with_extension("zip.sha256"))?
    );
    Ok(())
}

#[test]
fn existing_outputs_are_never_replaced() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let before = fs::read(&archive)?;
    assert_eq!(
        fixture
            .package(LINUX, "dist")
            .err()
            .map(|error| error.to_string()),
        Some("OUTPUT_EXISTS: Release output already exists".to_owned())
    );
    assert_eq!(fs::read(&archive)?, before);
    Ok(())
}

#[test]
fn executable_headers_must_match_the_declared_target() -> TestResult {
    for target in TARGETS {
        assert!(check_bytes(&executable_header(target), target).is_ok());
    }
    let linux = executable_header(LINUX);
    assert_eq!(
        code(check_bytes(&linux, "aarch64-unknown-linux-gnu")),
        Some("BINARY_TARGET_MISMATCH")
    );
    assert_eq!(
        code(check_bytes(&linux, "x86_64-apple-darwin")),
        Some("BINARY_TARGET_MISMATCH")
    );
    assert_eq!(
        code(check_bytes(
            &executable_header("aarch64-apple-darwin"),
            "x86_64-apple-darwin"
        )),
        Some("BINARY_TARGET_MISMATCH")
    );
    let mut library = executable_header("x86_64-pc-windows-msvc");
    library[150..152].copy_from_slice(&0x2002u16.to_le_bytes());
    assert_eq!(
        code(check_bytes(&library, "x86_64-pc-windows-msvc")),
        Some("BINARY_TARGET_MISMATCH")
    );
    assert_eq!(
        code(check_bytes(&linux[..63], LINUX)),
        Some("INVALID_BINARY")
    );
    assert_eq!(
        code(check_bytes(&linux, "riscv64gc-unknown-linux-gnu")),
        Some("UNSUPPORTED_TARGET")
    );
    let fixture = Fixture::new()?;
    let windows = executable_header("x86_64-pc-windows-msvc");
    assert_eq!(
        package_with(&fixture, &windows, LINUX)
            .err()
            .map(|error| error.to_string()),
        Some(
            "BINARY_TARGET_MISMATCH: Executable format or architecture does not match the target"
                .to_owned()
        )
    );
    assert!(
        !fixture
            .root
            .join("custom")
            .join(format!("{}.zip", base(LINUX)?))
            .exists()
    );
    Ok(())
}

#[test]
fn packaging_requires_every_nonempty_release_resource() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join("NOTICES"), b"")?;
    assert!(fixture.package(LINUX, "empty-notices").is_err());
    let fixture = Fixture::new()?;
    fs::remove_file(fixture.root.join("BETA.md"))?;
    assert!(fixture.package(LINUX, "missing-guide").is_err());
    let fixture = Fixture::new()?;
    fs::remove_file(fixture.root.join("schemas/ir/v1/document-ir.json"))?;
    assert!(fixture.package(LINUX, "missing-schema").is_err());
    Ok(())
}

#[test]
fn tampered_member_content_is_detected() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let readme = member(LINUX, "README.md")?;
    rewrite_archive(&archive, |entries| {
        for entry in entries.iter_mut().filter(|entry| entry.name == readme) {
            entry.bytes[0] ^= 1;
        }
    })?;
    assert_eq!(
        code(verify_archive(&archive)),
        Some("RELEASE_DIGEST_MISMATCH")
    );
    Ok(())
}

#[test]
fn extra_missing_duplicate_and_unsafe_members_are_rejected() -> TestResult {
    let cases: Vec<(&str, ArchiveEdit)> = vec![
        (
            "RELEASE_CONTENT_MISMATCH",
            Box::new(|entries, base| {
                entries.push(Entry {
                    name: format!("{base}/extra.txt"),
                    bytes: b"extra".to_vec(),
                    mode: 0o644,
                })
            }),
        ),
        (
            "RELEASE_METADATA_MISMATCH",
            Box::new(|entries, base| {
                entries.retain(|entry| entry.name != format!("{base}/README.md"))
            }),
        ),
        (
            "INVALID_ARCHIVE_MEMBER",
            Box::new(|entries, base| {
                entries.push(Entry {
                    name: format!("{base}/readme.md"),
                    bytes: b"collision".to_vec(),
                    mode: 0o644,
                })
            }),
        ),
        (
            "UNSAFE_ARCHIVE_PATH",
            Box::new(|entries, base| {
                entries.push(Entry {
                    name: format!("{base}/../escape.txt"),
                    bytes: b"escape".to_vec(),
                    mode: 0o644,
                })
            }),
        ),
        (
            "RELEASE_METADATA_MISMATCH",
            Box::new(|entries, base| {
                for entry in entries
                    .iter_mut()
                    .filter(|entry| entry.name == format!("{base}/README.md"))
                {
                    entry.mode = 0o755;
                }
            }),
        ),
    ];
    for (expected, edit) in cases {
        let fixture = Fixture::new()?;
        let archive = fixture.package(LINUX, "dist")?;
        let base = base(LINUX)?;
        rewrite_archive(&archive, |entries| edit(entries, &base))?;
        assert_eq!(code(verify_archive(&archive)), Some(expected));
    }
    Ok(())
}

#[test]
fn archive_name_root_and_manifest_identity_must_agree() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let renamed = archive.with_file_name("docsight-9.9.9-x86_64-unknown-linux-gnu.zip");
    fs::copy(&archive, &renamed)?;
    assert_eq!(
        code(verify_archive(&renamed)),
        Some("ARCHIVE_IDENTITY_MISMATCH")
    );
    Ok(())
}

#[test]
fn corrupted_compressed_streams_return_typed_errors() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let mut bytes = fs::read(&archive)?;
    let start = bytes.len() / 4;
    for byte in &mut bytes[start..start + 64] {
        *byte ^= 0x5a;
    }
    fs::write(&archive, bytes)?;
    let failure = code(verify_archive(&archive));
    assert!(
        matches!(
            failure,
            Some("CORRUPT_ARCHIVE" | "RELEASE_DIGEST_MISMATCH" | "INVALID_ARCHIVE_MEMBER")
        ),
        "{failure:?}"
    );
    Ok(())
}

#[test]
fn sidecars_bind_the_archive_bytes() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let sidecar = archive.with_extension("zip.sha256");
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("name")?;
    fs::remove_file(&sidecar)?;
    fs::write(&sidecar, format!("{}  {name}\n", "0".repeat(64)))?;
    assert_eq!(
        code(verify_sidecar(&archive)),
        Some("ARCHIVE_CHECKSUM_MISMATCH")
    );
    Ok(())
}

#[test]
fn extraction_reverifies_content_and_preserves_the_manifest() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let destination = fixture.root.join("extracted");
    let (binary, manifest) = extract_verified(&archive, &destination)?;
    assert_eq!(binary, destination.join("docsight"));
    for file in &manifest.files {
        assert_eq!(sha256_file(&destination.join(&file.path))?, file.sha256);
    }
    assert!(destination.join("release-manifest.json").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&binary)?.permissions().mode() & 0o777, 0o755);
    }
    Ok(())
}

#[test]
fn extraction_never_removes_a_preexisting_destination() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let destination = fixture.root.join("occupied");
    fs::create_dir(&destination)?;
    fs::write(destination.join("keep.txt"), b"user data")?;
    assert!(extract_verified(&archive, &destination).is_err());
    assert_eq!(fs::read(destination.join("keep.txt"))?, b"user data");
    Ok(())
}

fn release_set(fixture: &Fixture, directory: &str) -> TestResult<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for target in TARGETS {
        let archive = fixture.package(target, directory)?;
        fixture.receipt(&archive)?;
        fixture.signature(&archive)?;
        archives.push(archive);
    }
    Ok(archives)
}

fn receipt_path(archive: &Path, target: &str) -> TestResult<PathBuf> {
    Ok(archive
        .parent()
        .ok_or("parent")?
        .join(format!("smoke-{target}.json")))
}

#[test]
fn release_set_requires_five_verified_archives_and_receipts() -> TestResult {
    let fixture = Fixture::new()?;
    let archives = release_set(&fixture, "dist")?;
    let directory = fixture.root.join("dist");
    let summary = collect(&directory, &fixture.root)?;
    assert_eq!(summary["revision"], REVISION);
    assert_eq!(summary["targets"].as_array().ok_or("targets")?.len(), 5);
    let sums = fs::read_to_string(directory.join("SHA256SUMS"))?;
    assert_eq!(sums.lines().count(), 5);
    for archive in &archives {
        assert!(sums.contains(&sha256_file(archive)?));
    }
    assert_eq!(
        code(collect(&directory, &fixture.root)),
        Some("OUTPUT_EXISTS")
    );
    Ok(())
}

#[test]
fn release_set_rejects_missing_targets_and_unbound_receipts() -> TestResult {
    let fixture = Fixture::new()?;
    let archives = release_set(&fixture, "partial")?;
    fs::remove_file(&archives[0])?;
    assert_eq!(
        code(collect(&fixture.root.join("partial"), &fixture.root)),
        Some("INCOMPLETE_RELEASE_SET")
    );

    let fixture = Fixture::new()?;
    let archives = release_set(&fixture, "missing-receipt")?;
    fs::remove_file(receipt_path(&archives[1], TARGETS[1])?)?;
    assert!(collect(&fixture.root.join("missing-receipt"), &fixture.root).is_err());
    assert!(!fixture.root.join("missing-receipt/SHA256SUMS").exists());

    let fixture = Fixture::new()?;
    let archives = release_set(&fixture, "wrong-receipt")?;
    let path = receipt_path(&archives[2], TARGETS[2])?;
    let mut receipt: SmokeReceipt = serde_json::from_slice(&fs::read(&path)?)?;
    receipt.archive_sha256 = "f".repeat(64);
    fs::remove_file(&path)?;
    save(&path, &receipt)?;
    assert_eq!(
        code(collect(&fixture.root.join("wrong-receipt"), &fixture.root)),
        Some("INVALID_SMOKE_RECEIPT")
    );
    Ok(())
}

#[test]
fn smoke_receipts_must_attest_every_check_once_in_order() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(LINUX, "dist")?;
    let receipt = fixture.receipt(&archive)?;
    let manifest = verify_archive(&archive)?;
    let hash = sha256_file(&archive)?;
    assert!(validate_smoke_receipt(&receipt, &manifest, &hash).is_ok());

    let mut reordered = receipt.clone();
    reordered.checks.swap(0, 1);
    assert_eq!(
        code(validate_smoke_receipt(&reordered, &manifest, &hash)),
        Some("FAILED_SMOKE_RECEIPT")
    );
    let mut failed = receipt.clone();
    failed.checks[3].passed = false;
    assert_eq!(
        code(validate_smoke_receipt(&failed, &manifest, &hash)),
        Some("FAILED_SMOKE_RECEIPT")
    );
    let mut incomplete = receipt.clone();
    incomplete.checks.pop();
    assert_eq!(
        code(validate_smoke_receipt(&incomplete, &manifest, &hash)),
        Some("INCOMPLETE_SMOKE_RECEIPT")
    );
    let mut relabeled = receipt.clone();
    relabeled.passed = false;
    assert_eq!(
        code(validate_smoke_receipt(&relabeled, &manifest, &hash)),
        Some("INVALID_SMOKE_RECEIPT")
    );
    assert_eq!(
        code(validate_smoke_receipt(&receipt, &manifest, &"e".repeat(64))),
        Some("INVALID_SMOKE_RECEIPT")
    );
    Ok(())
}

#[test]
fn repository_matrix_declares_exactly_the_five_release_targets() -> TestResult {
    let declared = matrix(&workspace_root())?;
    let targets: BTreeSet<_> = declared
        .include
        .iter()
        .map(|entry| entry.target.as_str())
        .collect();
    assert_eq!(targets, TARGETS.into_iter().collect());
    Ok(())
}

#[test]
fn invalid_matrices_fail_closed() -> TestResult {
    let variants = [
        (
            json!({"include": [
                {"target": LINUX, "runner": "ubuntu-24.04", "binary": "docsight"},
                {"target": LINUX, "runner": "ubuntu-24.04", "binary": "docsight"}
            ]}),
            "INVALID_MATRIX",
        ),
        (
            json!({"include": [{"target": LINUX, "runner": "${{ inputs.runner }}", "binary": "docsight"}]}),
            "INVALID_RUNNER",
        ),
        (
            json!({"include": [{"target": LINUX, "runner": "ubuntu-24.04", "binary": "docsight"}]}),
            "INCOMPLETE_MATRIX",
        ),
        (
            json!({"include": [{"target": LINUX, "runner": "ubuntu-24.04", "binary": "docsight.exe"}]}),
            "INVALID_MATRIX",
        ),
    ];
    for (value, expected) in variants {
        let fixture = Fixture::new()?;
        let path = fixture.root.join("release/targets.json");
        fs::remove_file(&path)?;
        save(&path, &value)?;
        assert_eq!(code(matrix(&fixture.root)), Some(expected));
    }
    Ok(())
}
