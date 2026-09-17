use crate::tooling::common::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
struct Metadata {
    version: u64,
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    resolve: Option<Value>,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    manifest_path: PathBuf,
    license: Option<String>,
    license_file: Option<String>,
    source: Option<String>,
}

pub fn locked_packages(path: &Path) -> Result<BTreeSet<(String, String, Option<String>)>> {
    let bytes = read_bytes(path, 4_194_304)?;
    let mut packages = BTreeSet::new();
    let mut fields = BTreeMap::new();
    let mut in_package = false;
    for line in text(&bytes)?.lines().chain(std::iter::once("[end]")) {
        let line = line.trim();
        if line.starts_with('[') {
            if in_package {
                let name = fields.remove("name").ok_or_else(|| {
                    ToolError::new("INVALID_LOCKFILE", "Locked package has no name")
                })?;
                let version = fields.remove("version").ok_or_else(|| {
                    ToolError::new("INVALID_LOCKFILE", "Locked package has no version")
                })?;
                require(
                    packages.insert((name, version, fields.remove("source"))),
                    "INVALID_LOCKFILE",
                    "Locked package is duplicated",
                )?;
                fields.clear();
            }
            in_package = line == "[[package]]";
        } else if in_package && let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if ["name", "version", "source"].contains(&key) {
                let value = parse_json(value.trim().as_bytes())?;
                require(
                    fields
                        .insert(key.to_owned(), string(&value)?.to_owned())
                        .is_none(),
                    "INVALID_LOCKFILE",
                    "Locked identity field is duplicated",
                )?;
            }
        }
    }
    require(
        !packages.is_empty(),
        "INVALID_LOCKFILE",
        "Lockfile has no package identities",
    )?;
    Ok(packages)
}

fn license_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    require(
        !relative.as_os_str().is_empty(),
        "INVALID_LICENSE_PATH",
        "License resource path is empty",
    )?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ToolError::new(
                "INVALID_LICENSE_PATH",
                "License resources must remain inside their package",
            ));
        };
        path.push(name);
        require(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "INVALID_LICENSE_PATH",
            "License resources cannot follow symlinks",
        )?;
    }
    require(
        path.is_file() && path.canonicalize()?.starts_with(root),
        "INVALID_LICENSE_PATH",
        "License must be a contained regular file",
    )?;
    Ok(path)
}

fn license_files(package: &Package) -> Result<(PathBuf, Vec<PathBuf>)> {
    require(
        package.manifest_path.is_absolute()
            && fs::symlink_metadata(&package.manifest_path)?.is_file(),
        "INVALID_CARGO_METADATA",
        "Package manifest must be a regular file at an absolute path",
    )?;
    let directory = package
        .manifest_path
        .parent()
        .ok_or_else(|| {
            ToolError::new(
                "INVALID_CARGO_METADATA",
                "Package manifest has no directory",
            )
        })?
        .canonicalize()?;
    let mut paths = BTreeSet::new();
    if let Some(declared) = &package.license_file {
        paths.insert(license_path(&directory, Path::new(declared))?);
    }
    let mut inspected = 0usize;
    for item in fs::read_dir(&directory)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().to_uppercase();
        if ["LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            require(
                !item.file_type()?.is_symlink(),
                "INVALID_LICENSE_PATH",
                "License directories cannot follow symlinks",
            )?;
            let candidates = if item.file_type()?.is_dir() {
                list_files(&item.path(), 512)?
            } else {
                vec![item.path()]
            };
            inspected = inspected.checked_add(candidates.len()).ok_or_else(|| {
                ToolError::new("LICENSE_RESOURCE_LIMIT", "License resource count overflow")
            })?;
            require(
                inspected <= 512,
                "LICENSE_RESOURCE_LIMIT",
                "License resources exceed the traversal limit",
            )?;
            for path in candidates {
                require(
                    fs::symlink_metadata(&path)?.is_file(),
                    "INVALID_LICENSE_PATH",
                    "License must be a regular file",
                )?;
                paths.insert(path);
            }
        }
    }
    require(
        !paths.is_empty() && paths.len() <= 128,
        "MISSING_LICENSE_TEXT",
        "A bounded set of distributable license texts is required",
    )?;
    Ok((directory, paths.into_iter().collect()))
}

pub fn generate(metadata: Value, lock_path: &Path) -> Result<String> {
    let metadata: Metadata = decode(metadata)?;
    require(
        metadata.version == 1 && metadata.resolve.as_ref().is_some_and(Value::is_object),
        "INCOMPLETE_CARGO_METADATA",
        "Cargo metadata format version 1 with a resolved graph is required",
    )?;
    let locked = locked_packages(lock_path)?;
    let members: BTreeSet<_> = metadata.workspace_members.into_iter().collect();
    let mut seen = BTreeSet::new();
    let mut packages = Vec::new();
    for package in metadata.packages {
        require(
            seen.insert(package.id.clone()),
            "INVALID_CARGO_METADATA",
            "Package identities must be unique",
        )?;
        if members.contains(&package.id) {
            continue;
        }
        require(
            locked.contains(&(
                package.name.clone(),
                package.version.clone(),
                package.source.clone(),
            )),
            "UNLOCKED_DEPENDENCY",
            "Dependency identity and source must match Cargo.lock",
        )?;
        require(
            !package.name.is_empty()
                && package
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "INVALID_PACKAGE_NAME",
            "Dependency name is not a Cargo identifier",
        )?;
        checked_version(&package.version)?;
        require(
            package
                .license
                .as_ref()
                .is_none_or(|s| !s.contains(['\n', '\r', '\0']))
                && (package.license.as_ref().is_some_and(|s| !s.is_empty())
                    || package.license_file.is_some()),
            "MISSING_LICENSE_METADATA",
            "Dependency must declare a single-line license expression or license file",
        )?;
        packages.push(package);
    }
    require(
        !packages.is_empty(),
        "EMPTY_DEPENDENCY_NOTICES",
        "Resolved dependency notices cannot be empty",
    )?;
    packages.sort_by(|a, b| (&a.name, &a.version, &a.id).cmp(&(&b.name, &b.version, &b.id)));
    let mut output = String::from(
        "# Third-party notices\n\nGenerated from locked Cargo workspace metadata, including enabled build and test dependencies.\nThis inventory is not a claim that every listed package is linked into the runtime binary.\n\n",
    );
    let mut total = 0usize;
    for package in packages {
        output.push_str(&format!(
            "## {} {}\n\nDeclared license: {}\n\n",
            package.name,
            package.version,
            package
                .license
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("license-file")
        ));
        let (directory, files) = license_files(&package)?;
        for file in files {
            let bytes = read_bytes(&file, MAX_JSON_BYTES)?;
            total = total.checked_add(bytes.len()).ok_or_else(|| {
                ToolError::new("LICENSE_SIZE_LIMIT", "License byte count overflow")
            })?;
            require(
                !bytes.is_empty() && total <= 16_777_216,
                "LICENSE_SIZE_LIMIT",
                "License text exceeds the distribution limit",
            )?;
            let relative = file
                .strip_prefix(&directory)
                .map_err(|_| ToolError::new("INVALID_LICENSE_PATH", "License escaped its package"))?
                .to_string_lossy()
                .replace('\\', "/");
            output.push_str(&format!(
                "### {relative}\n\n{}\n\n",
                text(&bytes)?.trim_end()
            ));
        }
    }
    Ok(output)
}
