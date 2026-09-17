#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::*;
use xtask::release::TARGETS;
use xtask::release::notices::{generate, locked_packages};
use xtask::tooling::common::{parse_json, workspace_root};

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

struct Registry {
    root: PathBuf,
    _directory: tempfile::TempDir,
}

impl Registry {
    fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        Ok(Self {
            root,
            _directory: directory,
        })
    }

    fn crate_directory(&self, name: &str, version: &str) -> TestResult<PathBuf> {
        let directory = self.root.join(format!("{name}-{version}"));
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )?;
        Ok(directory)
    }

    fn lockfile(&self, packages: &[(&str, &str)]) -> TestResult<PathBuf> {
        let mut text =
            String::from("version = 4\n\n[[package]]\nname = \"member\"\nversion = \"0.1.0\"\n");
        for (name, version) in packages {
            text.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"{REGISTRY}\"\n"
            ));
        }
        let path = self.root.join("Cargo.lock");
        fs::write(&path, text)?;
        Ok(path)
    }
}

fn package(directory: &Path, name: &str, version: &str, license: Option<&str>) -> Value {
    json!({
        "id": format!("{REGISTRY}#{name}@{version}"),
        "name": name,
        "version": version,
        "manifest_path": directory.join("Cargo.toml"),
        "license": license,
        "license_file": null,
        "source": REGISTRY,
    })
}

fn metadata(packages: Vec<Value>) -> Value {
    let mut all = vec![json!({
        "id": "path+file:///workspace#member@0.1.0",
        "name": "member",
        "version": "0.1.0",
        "manifest_path": "/workspace/Cargo.toml",
        "license": "MIT",
        "license_file": null,
        "source": null,
    })];
    all.extend(packages);
    json!({
        "version": 1,
        "packages": all,
        "workspace_members": ["path+file:///workspace#member@0.1.0"],
        "resolve": {"nodes": [], "root": null},
    })
}

#[test]
fn notices_include_license_texts_in_canonical_order_without_host_paths() -> TestResult {
    let registry = Registry::new()?;
    let zeta = registry.crate_directory("zeta", "2.0.0")?;
    fs::write(zeta.join("LICENSE-MIT"), "MIT license text for zeta\n")?;
    let alpha = registry.crate_directory("alpha", "1.0.0")?;
    fs::write(alpha.join("LICENSE"), "Apache license text for alpha\n")?;
    fs::create_dir(alpha.join("LICENSES"))?;
    fs::write(alpha.join("LICENSES/extra.txt"), "Additional notice\n")?;
    let lock = registry.lockfile(&[("alpha", "1.0.0"), ("zeta", "2.0.0")])?;
    let notices = generate(
        metadata(vec![
            package(&zeta, "zeta", "2.0.0", Some("MIT")),
            package(&alpha, "alpha", "1.0.0", Some("Apache-2.0")),
        ]),
        &lock,
    )?;
    let alpha_heading = notices.find("## alpha 1.0.0").ok_or("alpha")?;
    let zeta_heading = notices.find("## zeta 2.0.0").ok_or("zeta")?;
    assert!(alpha_heading < zeta_heading);
    assert!(notices.contains("Declared license: Apache-2.0"));
    assert!(notices.contains("### LICENSES/extra.txt\n\nAdditional notice"));
    assert!(notices.contains("MIT license text for zeta"));
    assert!(!notices.contains("## member"));
    assert!(!notices.contains(registry.root.to_string_lossy().as_ref()));
    Ok(())
}

#[test]
fn packages_without_distributable_license_text_fail_closed() -> TestResult {
    let registry = Registry::new()?;
    let bare = registry.crate_directory("bare", "1.0.0")?;
    fs::write(bare.join("README.md"), "no license file\n")?;
    let lock = registry.lockfile(&[("bare", "1.0.0")])?;
    assert_eq!(
        code(generate(
            metadata(vec![package(&bare, "bare", "1.0.0", Some("MIT"))]),
            &lock
        )),
        Some("MISSING_LICENSE_TEXT")
    );
    Ok(())
}

#[test]
fn packages_must_declare_license_metadata_and_match_the_lockfile() -> TestResult {
    let registry = Registry::new()?;
    let crate_directory = registry.crate_directory("alpha", "1.0.0")?;
    fs::write(crate_directory.join("LICENSE"), "text\n")?;
    let lock = registry.lockfile(&[("alpha", "1.0.0")])?;
    assert_eq!(
        code(generate(
            metadata(vec![package(
                &crate_directory,
                "alpha",
                "1.0.1",
                Some("MIT")
            )]),
            &lock
        )),
        Some("UNLOCKED_DEPENDENCY")
    );
    assert_eq!(
        code(generate(
            metadata(vec![package(&crate_directory, "alpha", "1.0.0", None)]),
            &lock
        )),
        Some("MISSING_LICENSE_METADATA")
    );
    assert_eq!(
        code(generate(
            metadata(vec![package(
                &crate_directory,
                "alpha",
                "1.0.0",
                Some("MIT\nforged")
            )]),
            &lock
        )),
        Some("MISSING_LICENSE_METADATA")
    );
    let mut unresolved = metadata(vec![package(
        &crate_directory,
        "alpha",
        "1.0.0",
        Some("MIT"),
    )]);
    unresolved["resolve"] = Value::Null;
    assert_eq!(
        code(generate(unresolved, &lock)),
        Some("INCOMPLETE_CARGO_METADATA")
    );
    assert_eq!(
        code(generate(metadata(Vec::new()), &lock)),
        Some("EMPTY_DEPENDENCY_NOTICES")
    );
    Ok(())
}

#[test]
fn declared_license_files_cannot_escape_their_package() -> TestResult {
    let registry = Registry::new()?;
    let crate_directory = registry.crate_directory("alpha", "1.0.0")?;
    fs::write(registry.root.join("outside.txt"), "outside\n")?;
    let lock = registry.lockfile(&[("alpha", "1.0.0")])?;
    let mut entry = package(&crate_directory, "alpha", "1.0.0", None);
    entry["license_file"] = json!("../outside.txt");
    assert_eq!(
        code(generate(metadata(vec![entry]), &lock)),
        Some("INVALID_LICENSE_PATH")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_license_resources_are_rejected_before_traversal() -> TestResult {
    let registry = Registry::new()?;
    let crate_directory = registry.crate_directory("alpha", "1.0.0")?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret"), "private\n")?;
    std::os::unix::fs::symlink(outside.path(), crate_directory.join("LICENSES"))?;
    let lock = registry.lockfile(&[("alpha", "1.0.0")])?;
    assert_eq!(
        code(generate(
            metadata(vec![package(
                &crate_directory,
                "alpha",
                "1.0.0",
                Some("MIT")
            )]),
            &lock
        )),
        Some("INVALID_LICENSE_PATH")
    );
    Ok(())
}

#[test]
fn lockfile_identities_are_parsed_strictly() -> TestResult {
    let registry = Registry::new()?;
    let path = registry.lockfile(&[("alpha", "1.0.0"), ("alpha", "2.0.0")])?;
    let packages = locked_packages(&path)?;
    assert!(packages.contains(&("alpha".into(), "2.0.0".into(), Some(REGISTRY.into()))));
    assert!(packages.contains(&("member".into(), "0.1.0".into(), None)));
    let duplicated = registry.root.join("duplicated.lock");
    fs::write(
        &duplicated,
        "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n",
    )?;
    assert_eq!(code(locked_packages(&duplicated)), Some("INVALID_LOCKFILE"));
    let nameless = registry.root.join("nameless.lock");
    fs::write(&nameless, "[[package]]\nversion = \"1.0.0\"\n")?;
    assert_eq!(code(locked_packages(&nameless)), Some("INVALID_LOCKFILE"));
    Ok(())
}

fn workspace_metadata(target: &str) -> TestResult<Value> {
    let cargo = std::env::var_os("CARGO").ok_or("CARGO is not set by the test harness")?;
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--all-features",
            "--filter-platform",
            target,
        ])
        .current_dir(workspace_root())
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(parse_json(&output.stdout)?)
}

#[test]
fn workspace_notices_are_complete_for_every_release_target() -> TestResult {
    let root = workspace_root();
    for target in TARGETS {
        let notices = generate(workspace_metadata(target)?, &root.join("Cargo.lock"))?;
        assert!(notices.contains("## serde "), "{target}");
        assert!(
            !notices.contains(root.to_string_lossy().as_ref()),
            "{target}"
        );
    }
    Ok(())
}
