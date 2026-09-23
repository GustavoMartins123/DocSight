use crate::DocsightError;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::{Builder, NamedTempFile};

pub fn validate_artifact_paths(sources: &[&Path], outputs: &[&Path]) -> Result<(), DocsightError> {
    let mut existing_sources = Vec::with_capacity(sources.len());
    for source in sources {
        if canonical_existing(source)?.is_some() {
            existing_sources.push(*source);
        }
    }
    let mut targets = Vec::with_capacity(outputs.len());

    for output in outputs {
        let target = prospective_output_path(output)?;
        match fs::symlink_metadata(output) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if aliases_any(output, &existing_sources)? {
                    return Err(artifact_conflict(output));
                }
                return Err(invalid_artifact(format!(
                    "artifact output must not be a symbolic link: {}",
                    output.display()
                )));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(invalid_artifact(format!(
                    "artifact output must be a regular file: {}",
                    output.display()
                )));
            }
            Ok(metadata) => {
                if aliases_any(output, &existing_sources)? {
                    return Err(artifact_conflict(output));
                }
                if metadata.permissions().readonly() {
                    return Err(invalid_artifact(format!(
                        "artifact output is read-only: {}",
                        output.display()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(DocsightError::Io {
                    path: output.to_path_buf(),
                    source,
                });
            }
        }
        targets.push(target);
    }

    for left in 0..outputs.len() {
        for right in left + 1..outputs.len() {
            if targets[left] == targets[right] || aliases_any(outputs[left], &[outputs[right]])? {
                return Err(invalid_artifact(format!(
                    "artifact outputs must be distinct: {} and {}",
                    outputs[left].display(),
                    outputs[right].display()
                )));
            }
        }
    }

    Ok(())
}

pub fn validate_new_artifact_directory(path: &Path) -> Result<(), DocsightError> {
    let target = prospective_output_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Err(invalid_artifact(format!(
            "artifact output directory already exists: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = target.parent().ok_or_else(|| {
                invalid_artifact("artifact output directory has no parent".to_owned())
            })?;
            let metadata = fs::metadata(parent).map_err(|source| DocsightError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
            if !metadata.is_dir() {
                return Err(invalid_artifact(format!(
                    "artifact output parent is not a directory: {}",
                    parent.display()
                )));
            }
            Ok(())
        }
        Err(source) => Err(DocsightError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn write_all(path: &Path, bytes: &[u8]) -> Result<(), DocsightError> {
    validate_artifact_paths(&[], &[path])?;
    let target = prospective_output_path(path)?;
    let parent = target
        .parent()
        .ok_or_else(|| invalid_artifact("artifact output has no parent directory".to_owned()))?;
    let temporary = stage_file(parent, bytes)?;
    temporary
        .persist(&target)
        .map_err(|error| DocsightError::Io {
            path: target.clone(),
            source: error.error,
        })?;
    Ok(())
}

pub fn write_all_group(entries: &[(&Path, &[u8])]) -> Result<(), DocsightError> {
    write_all_group_impl(entries, None)
}

fn write_all_group_impl(
    entries: &[(&Path, &[u8])],
    fail_publish_at: Option<usize>,
) -> Result<(), DocsightError> {
    if entries.is_empty() {
        return Ok(());
    }
    let outputs = entries.iter().map(|(path, _)| *path).collect::<Vec<_>>();
    validate_artifact_paths(&[], &outputs)?;

    let mut staged = Vec::with_capacity(entries.len());
    let mut targets = Vec::with_capacity(entries.len());
    for (path, bytes) in entries {
        let target = prospective_output_path(path)?;
        let parent = target.parent().ok_or_else(|| {
            invalid_artifact("artifact output has no parent directory".to_owned())
        })?;
        staged.push(stage_file(parent, bytes)?);
        targets.push(target);
    }

    let mut backups = Vec::with_capacity(targets.len());
    for target in &targets {
        let backup = if target.exists() {
            let parent = target.parent().ok_or_else(|| {
                invalid_artifact("artifact output has no parent directory".to_owned())
            })?;
            Some(backup_file(target, parent)?)
        } else {
            None
        };
        backups.push(backup);
    }

    let mut published = 0_usize;
    for (index, (target, temporary)) in targets.iter().zip(staged).enumerate() {
        if fail_publish_at == Some(index) {
            let publish_error = DocsightError::Io {
                path: target.clone(),
                source: io::Error::other("injected artifact publication failure"),
            };
            return Err(rollback_group(&targets, backups, published, publish_error));
        }
        match temporary.persist(target) {
            Ok(_) => published = index + 1,
            Err(error) => {
                let publish_error = DocsightError::Io {
                    path: targets[index].clone(),
                    source: error.error,
                };
                return Err(rollback_group(&targets, backups, published, publish_error));
            }
        }
    }

    for backup in backups.into_iter().flatten() {
        let path = backup.path().to_path_buf();
        backup
            .close()
            .map_err(|source| DocsightError::Io { path, source })?;
    }
    Ok(())
}

pub fn write_directory_atomic(
    path: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), DocsightError> {
    validate_new_artifact_directory(path)?;
    let target = prospective_output_path(path)?;
    let parent = target
        .parent()
        .ok_or_else(|| invalid_artifact("artifact output directory has no parent".to_owned()))?;
    let staging = Builder::new()
        .prefix(".docsight-artifacts-")
        .tempdir_in(parent)
        .map_err(|source| DocsightError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

    for (name, bytes) in files {
        validate_member_name(name)?;
        let destination = staging.path().join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|source| DocsightError::Io {
                path: destination.clone(),
                source,
            })?;
        file.write_all(bytes).map_err(|source| DocsightError::Io {
            path: destination.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| DocsightError::Io {
            path: destination.clone(),
            source,
        })?;
    }

    let staging_path = staging.keep();
    if let Err(source) = fs::symlink_metadata(&target) {
        if source.kind() != io::ErrorKind::NotFound {
            let cleanup = fs::remove_dir_all(&staging_path);
            let mut message = source.to_string();
            if let Err(cleanup_error) = cleanup {
                message.push_str(&format!("; staging cleanup failed: {cleanup_error}"));
            }
            return Err(DocsightError::Io {
                path: target,
                source: io::Error::other(message),
            });
        }
    } else {
        let cleanup = fs::remove_dir_all(&staging_path);
        let message = match cleanup {
            Ok(()) => invalid_artifact(format!(
                "artifact output directory already exists: {}",
                target.display()
            )),
            Err(cleanup_error) => DocsightError::Io {
                path: staging_path,
                source: io::Error::other(format!(
                    "artifact output directory already exists: {}; staging cleanup failed: {cleanup_error}",
                    target.display()
                )),
            },
        };
        return Err(message);
    }
    if let Err(source) = fs::rename(&staging_path, &target) {
        let cleanup = fs::remove_dir_all(&staging_path);
        let mut message = source.to_string();
        if let Err(cleanup_error) = cleanup {
            message.push_str(&format!("; staging cleanup failed: {cleanup_error}"));
        }
        return Err(DocsightError::Io {
            path: target,
            source: io::Error::other(message),
        });
    }
    Ok(())
}

fn canonical_existing(path: &Path) -> Result<Option<PathBuf>, DocsightError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DocsightError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn prospective_output_path(path: &Path) -> Result<PathBuf, DocsightError> {
    let name = path.file_name().ok_or_else(|| {
        invalid_artifact(format!(
            "artifact output must name a file or directory: {}",
            path.display()
        ))
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|source| DocsightError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let metadata = fs::metadata(&parent).map_err(|source| DocsightError::Io {
        path: parent.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(invalid_artifact(format!(
            "artifact output parent is not a directory: {}",
            parent.display()
        )));
    }
    Ok(parent.join(name))
}

fn aliases_any(left: &Path, rights: &[&Path]) -> Result<bool, DocsightError> {
    for right in rights {
        match same_file::is_same_file(left, right) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(DocsightError::Io {
                    path: left.to_path_buf(),
                    source,
                });
            }
        }
    }
    Ok(false)
}

fn artifact_conflict(path: &Path) -> DocsightError {
    invalid_artifact(format!(
        "artifact output must not alias the source document: {}",
        path.display()
    ))
}

fn invalid_artifact(message: String) -> DocsightError {
    DocsightError::InvalidArgument { message }
}

fn stage_file(parent: &Path, bytes: &[u8]) -> Result<NamedTempFile, DocsightError> {
    let mut temporary = Builder::new()
        .prefix(".docsight-stage-")
        .tempfile_in(parent)
        .map_err(|source| DocsightError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(bytes)
        .map_err(|source| DocsightError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| DocsightError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    Ok(temporary)
}

fn backup_file(target: &Path, parent: &Path) -> Result<NamedTempFile, DocsightError> {
    let mut source = File::open(target).map_err(|source| DocsightError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    let mut temporary = Builder::new()
        .prefix(".docsight-backup-")
        .tempfile_in(parent)
        .map_err(|source| DocsightError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    io::copy(&mut source, &mut temporary).map_err(|source| DocsightError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| DocsightError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    Ok(temporary)
}

fn rollback_group(
    targets: &[PathBuf],
    mut backups: Vec<Option<NamedTempFile>>,
    published: usize,
    publish_error: DocsightError,
) -> DocsightError {
    for target in targets.iter().take(published).rev() {
        if let Err(error) = fs::remove_file(target)
            && error.kind() != io::ErrorKind::NotFound
        {
            return DocsightError::Io {
                path: target.clone(),
                source: io::Error::other(format!(
                    "artifact publish failed: {publish_error}; rollback failed: {error}"
                )),
            };
        }
    }
    for (target, backup) in targets.iter().zip(backups.drain(..published)) {
        if let Some(backup) = backup
            && let Err(error) = backup.persist(target)
        {
            return DocsightError::Io {
                path: target.clone(),
                source: io::Error::other(format!(
                    "artifact publish failed: {publish_error}; rollback failed: {}",
                    error.error
                )),
            };
        }
    }
    for backup in backups.into_iter().flatten() {
        let path = backup.path().to_path_buf();
        if let Err(error) = backup.close() {
            return DocsightError::Io {
                path,
                source: io::Error::other(format!(
                    "artifact publish failed: {publish_error}; backup cleanup failed: {error}"
                )),
            };
        }
    }
    publish_error
}

fn validate_member_name(name: &str) -> Result<(), DocsightError> {
    let path = Path::new(name);
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
        || name == "."
        || name == ".."
    {
        return Err(invalid_artifact(format!(
            "artifact directory member must be a single file name: {name}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_publish_failure_restores_targets_and_removes_staging_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, b"old first")?;
        fs::write(&second, b"old second")?;

        let error = write_all_group_impl(
            &[
                (&first, b"new first".as_slice()),
                (&second, b"new second".as_slice()),
            ],
            Some(1),
        );

        assert!(matches!(error, Err(DocsightError::Io { .. })));
        assert_eq!(fs::read(&first)?, b"old first");
        assert_eq!(fs::read(&second)?, b"old second");
        let staging_files = fs::read_dir(directory.path())?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".docsight-")
            })
            .count();
        assert_eq!(staging_files, 0);
        Ok(())
    }
}
