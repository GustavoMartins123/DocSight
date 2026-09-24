use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("validation")
        .join(name)
}

fn copy_fixture(directory: &TempDir, name: &str) -> Result<PathBuf, std::io::Error> {
    let path = directory.path().join(name);
    std::fs::copy(fixture(name), &path)?;
    Ok(path)
}

fn digest(path: &Path) -> Result<String, std::io::Error> {
    Ok(Sha256::digest(std::fs::read(path)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn assert_error(
    output: &std::process::Output,
    exit_code: i32,
    code: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(output.status.code(), Some(exit_code));
    assert!(output.stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(diagnostic["error"]["code"], code);
    assert!(
        !diagnostic["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(
        !diagnostic["error"]["effect"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[test]
fn artifact_commands_reject_source_output_collisions_without_modifying_source()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = copy_fixture(&directory, "sample_semantic.pdf")?;
    let before = digest(&source)?;
    let source_text = source.to_str().ok_or("source path")?;

    let render = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source_text,
            "--page",
            "1",
            "--out",
            source_text,
        ])
        .output()?;
    assert_error(&render, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let contact_sheet = docsight()
        .args([
            "--agent",
            "--json-errors",
            "contact-sheet",
            source_text,
            "--pages",
            "1",
            "--out",
            source_text,
        ])
        .output()?;
    assert_error(&contact_sheet, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let crop = docsight()
        .args([
            "--agent",
            "--json-errors",
            "crop",
            source_text,
            "--page",
            "1",
            "--bbox",
            "0,0,100,100",
            "--out",
            source_text,
        ])
        .output()?;
    assert_error(&crop, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let bundle = docsight()
        .args([
            "--agent",
            "--json-errors",
            "bundle",
            source_text,
            "--out",
            source_text,
        ])
        .output()?;
    assert_error(&bundle, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let sandbox = docsight()
        .args([
            "--agent",
            "--json-errors",
            "--sandbox",
            "render",
            source_text,
            "--page",
            "1",
            "--out",
            source_text,
        ])
        .output()?;
    assert_error(&sandbox, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);
    Ok(())
}

#[test]
fn artifact_commands_reject_relative_and_hardlink_source_aliases()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = copy_fixture(&directory, "sample_semantic.pdf")?;
    let before = digest(&source)?;

    let relative = docsight()
        .current_dir(directory.path())
        .args([
            "--agent",
            "--json-errors",
            "render",
            "sample_semantic.pdf",
            "--page",
            "1",
            "--out",
            source.to_str().ok_or("source path")?,
        ])
        .output()?;
    assert_error(&relative, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let hardlink = directory.path().join("source-hardlink.pdf");
    std::fs::hard_link(&source, &hardlink)?;
    let hardlink_output = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--out",
            hardlink.to_str().ok_or("hardlink path")?,
        ])
        .output()?;
    assert_error(&hardlink_output, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);

    let contact_sheet_hardlink = docsight()
        .args([
            "--agent",
            "--json-errors",
            "contact-sheet",
            source.to_str().ok_or("source path")?,
            "--pages",
            "1",
            "--out",
            hardlink.to_str().ok_or("hardlink path")?,
        ])
        .output()?;
    assert_error(&contact_sheet_hardlink, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn artifact_commands_reject_output_symlinks() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let source = copy_fixture(&directory, "sample_semantic.pdf")?;
    let output = directory.path().join("output.pdf");
    symlink(&source, &output)?;
    let before = digest(&source)?;

    let result = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--out",
            output.to_str().ok_or("output path")?,
        ])
        .output()?;
    assert_error(&result, 2, "USAGE")?;
    assert_eq!(digest(&source)?, before);
    Ok(())
}

#[test]
fn artifact_output_must_be_a_writable_regular_file() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = fixture("sample_semantic.pdf");
    let source_text = source.to_str().ok_or("source path")?;

    let output_directory = directory.path().join("output-directory");
    std::fs::create_dir(&output_directory)?;
    let marker = output_directory.join("marker");
    std::fs::write(&marker, b"preserve")?;
    let directory_result = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source_text,
            "--page",
            "1",
            "--out",
            output_directory.to_str().ok_or("output directory path")?,
        ])
        .output()?;
    assert_error(&directory_result, 2, "USAGE")?;
    assert_eq!(std::fs::read(&marker)?, b"preserve");

    let read_only = directory.path().join("read-only.png");
    std::fs::write(&read_only, b"preserve read-only output")?;
    let mut permissions = std::fs::metadata(&read_only)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&read_only, permissions)?;
    let read_only_result = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source_text,
            "--page",
            "1",
            "--out",
            read_only.to_str().ok_or("read-only output path")?,
        ])
        .output()?;
    assert_error(&read_only_result, 2, "USAGE")?;
    assert_eq!(std::fs::read(&read_only)?, b"preserve read-only output");
    Ok(())
}

#[test]
fn render_rejects_output_and_trace_aliases() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = fixture("sample_semantic.pdf");
    let output = directory.path().join("artifact.bin");

    let identical = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--out",
            output.to_str().ok_or("output path")?,
            "--trace",
            output.to_str().ok_or("output path")?,
        ])
        .output()?;
    assert_error(&identical, 2, "USAGE")?;
    assert!(!output.exists());

    std::fs::write(&output, b"old")?;
    let trace = directory.path().join("artifact.trace");
    std::fs::hard_link(&output, &trace)?;
    let hardlinked = docsight()
        .args([
            "--agent",
            "--json-errors",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--out",
            output.to_str().ok_or("output path")?,
            "--trace",
            trace.to_str().ok_or("trace path")?,
        ])
        .output()?;
    assert_error(&hardlinked, 2, "USAGE")?;
    assert_eq!(std::fs::read(&output)?, b"old");
    assert_eq!(std::fs::read(&trace)?, b"old");
    Ok(())
}

#[test]
fn render_atomically_replaces_output_and_trace() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = fixture("sample_semantic.pdf");
    let output = directory.path().join("page.png");
    let trace = directory.path().join("page.trace");
    std::fs::write(&output, b"old output")?;
    std::fs::write(&trace, b"old trace")?;

    let result = docsight()
        .args([
            "--agent",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--dpi",
            "72",
            "--out",
            output.to_str().ok_or("output path")?,
            "--trace",
            trace.to_str().ok_or("trace path")?,
        ])
        .output()?;
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stderr.is_empty());
    assert!(std::fs::read(&output)?.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(!std::fs::read(&trace)?.is_empty());
    let artifacts = std::fs::read_dir(directory.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with(".docsight-"))
        .count();
    assert_eq!(artifacts, 0);
    Ok(())
}

#[test]
fn output_limit_failure_preserves_render_and_bundle_artifacts()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = fixture("sample_semantic.pdf");
    let source_text = source.to_str().ok_or("source path")?;
    let png = directory.path().join("page.png");
    let trace = directory.path().join("page.trace");
    let bundle = directory.path().join("page.dse");
    std::fs::write(&png, b"old png")?;
    std::fs::write(&trace, b"old trace")?;
    std::fs::write(&bundle, b"old bundle")?;

    let render = docsight()
        .args([
            "--agent",
            "--json-errors",
            "--max-bytes",
            "512",
            "render",
            source_text,
            "--page",
            "1",
            "--out",
            png.to_str().ok_or("png path")?,
            "--trace",
            trace.to_str().ok_or("trace path")?,
        ])
        .output()?;
    assert_error(&render, 2, "USAGE")?;
    assert_eq!(std::fs::read(&png)?, b"old png");
    assert_eq!(std::fs::read(&trace)?, b"old trace");

    let bundled = docsight()
        .args([
            "--agent",
            "--json-errors",
            "--max-bytes",
            "768",
            "bundle",
            source_text,
            "--out",
            bundle.to_str().ok_or("bundle path")?,
        ])
        .output()?;
    assert_error(&bundled, 2, "USAGE")?;
    assert_eq!(std::fs::read(&bundle)?, b"old bundle");
    Ok(())
}

#[test]
fn sandbox_render_can_publish_output_and_trace() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let source = fixture("sample_semantic.pdf");
    let output = directory.path().join("sandbox.png");
    let trace = directory.path().join("sandbox.trace");

    let result = docsight()
        .args([
            "--agent",
            "--sandbox",
            "render",
            source.to_str().ok_or("source path")?,
            "--page",
            "1",
            "--dpi",
            "72",
            "--out",
            output.to_str().ok_or("output path")?,
            "--trace",
            trace.to_str().ok_or("trace path")?,
        ])
        .output()?;
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stderr.is_empty());
    assert!(std::fs::read(&output)?.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(!std::fs::read(&trace)?.is_empty());
    Ok(())
}

#[test]
fn visual_diff_rejects_existing_output_directory_without_clobber()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let before = fixture("sample_headings.docx");
    let after = fixture("sample_tables.docx");
    let output = directory.path().join("visual");
    std::fs::create_dir(&output)?;
    let marker = output.join("marker");
    std::fs::write(&marker, b"preserve")?;

    let result = docsight()
        .args([
            "--agent",
            "--json-errors",
            "diff",
            before.to_str().ok_or("before path")?,
            after.to_str().ok_or("after path")?,
            "--out-dir",
            output.to_str().ok_or("output path")?,
        ])
        .output()?;
    assert_error(&result, 2, "USAGE")?;
    assert_eq!(std::fs::read(&marker)?, b"preserve");
    assert_eq!(std::fs::read_dir(&output)?.count(), 1);
    Ok(())
}
