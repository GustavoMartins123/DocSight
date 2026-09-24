use crate::release::{
    Check, SMOKE_CHECKS, SmokeReceipt,
    archive::{extract_verified, verify_archive},
    native_target,
};
use crate::tooling::{
    common::*,
    process::{NativeRunner, ProcessLimits, ProcessResult, Runner},
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub fn agent_result(result: &ProcessResult) -> Result<Value> {
    let value = parse_json(&result.stdout)?;
    require(
        value.get("schema").and_then(Value::as_str) == Some("docsight.agent/v2")
            && value.get("result").is_some_and(Value::is_object),
        "SMOKE_PROTOCOL_MISMATCH",
        "Command did not return the canonical agent result envelope",
    )?;
    require(
        result.stderr.is_empty(),
        "SMOKE_STDERR_CONTAMINATION",
        "Successful agent command wrote unexpected stderr",
    )?;
    Ok(field(&value, "result")?.clone())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn png_error() -> ToolError {
    ToolError::new(
        "SMOKE_INVALID_PNG",
        "PNG is incomplete, corrupt or outside its bounds",
    )
}

pub fn validate_png_bytes(bytes: &[u8]) -> Result<()> {
    require(
        bytes.len() <= 67_108_864 && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "SMOKE_INVALID_PNG",
        "Render did not produce a bounded PNG",
    )?;
    let mut position = 8usize;
    let mut header = false;
    let mut data = false;
    let mut end = false;
    while position < bytes.len() {
        let head_end = position.checked_add(8).ok_or_else(png_error)?;
        let head = bytes.get(position..head_end).ok_or_else(png_error)?;
        let length = usize::try_from(u32::from_be_bytes(
            head[..4].try_into().map_err(|_| png_error())?,
        ))
        .map_err(|_| png_error())?;
        let content_end = head_end.checked_add(length).ok_or_else(png_error)?;
        let next = content_end.checked_add(4).ok_or_else(png_error)?;
        let content = bytes.get(head_end..content_end).ok_or_else(png_error)?;
        let expected = u32::from_be_bytes(
            bytes
                .get(content_end..next)
                .ok_or_else(png_error)?
                .try_into()
                .map_err(|_| png_error())?,
        );
        require(
            crc32(bytes.get(position + 4..content_end).ok_or_else(png_error)?) == expected,
            "SMOKE_INVALID_PNG",
            "PNG chunk checksum is invalid",
        )?;
        match &head[4..8] {
            b"IHDR" => {
                if header || position != 8 || length != 13 {
                    return Err(png_error());
                }
                let width = u32::from_be_bytes(content[..4].try_into().map_err(|_| png_error())?);
                let height = u32::from_be_bytes(content[4..8].try_into().map_err(|_| png_error())?);
                require(
                    (1..=16384).contains(&width)
                        && (1..=16384).contains(&height)
                        && u64::from(width) * u64::from(height) <= 16_777_216,
                    "SMOKE_INVALID_PNG",
                    "PNG dimensions exceed the smoke-test bounds",
                )?;
                header = true;
            }
            b"IDAT" => {
                if !header || end {
                    return Err(png_error());
                }
                data = true;
            }
            b"IEND" => {
                if !header || !data || length != 0 || next != bytes.len() {
                    return Err(png_error());
                }
                end = true;
            }
            _ => {
                if !header || end {
                    return Err(png_error());
                }
            }
        }
        position = next;
    }
    require(
        header && data && end,
        "SMOKE_INVALID_PNG",
        "PNG is missing required chunks",
    )?;
    docsight_core::decode_png(bytes).map_err(|_| png_error())?;
    Ok(())
}

pub fn validate_png(path: &Path) -> Result<()> {
    validate_png_bytes(&read_bytes(path, 67_108_864)?)
}

fn arguments(name: &str, binary: &Path, package: &Path, work: &Path) -> Result<Vec<OsString>> {
    let mut args = vec![binary.as_os_str().to_owned()];
    match name {
        "version" => args.push("--version".into()),
        "capabilities" => args.extend(["--agent".into(), "capabilities".into()]),
        "typed_error" => args.extend([
            "--json-errors".into(),
            "inspect".into(),
            work.join("invalid.docx").into_os_string(),
            "--json".into(),
        ]),
        "identical_diff" => {
            let input = package
                .join("examples/sample_headings.docx")
                .into_os_string();
            args.extend(["--agent".into(), "diff".into(), input.clone(), input]);
        }
        "sandbox_inspect" => args.extend([
            "--agent".into(),
            "--sandbox".into(),
            "inspect".into(),
            package
                .join("examples/sample_headings.docx")
                .into_os_string(),
        ]),
        name if name.starts_with("completion_") => args.extend([
            "completions".into(),
            name.trim_start_matches("completion_").into(),
        ]),
        name => {
            let (format, operation) = name
                .split_once('_')
                .ok_or_else(|| ToolError::new("UNKNOWN_SMOKE_CHECK", "Unknown smoke operation"))?;
            let file = match format {
                "docx" => "sample_headings.docx",
                "pdf" => "sample_semantic.pdf",
                _ => {
                    return Err(ToolError::new(
                        "UNKNOWN_SMOKE_CHECK",
                        "Unknown smoke format",
                    ));
                }
            };
            let operation = if operation == "determinism" {
                "inspect"
            } else {
                operation
            };
            args.extend([
                "--agent".into(),
                operation.into(),
                package.join("examples").join(file).into_os_string(),
            ]);
            if operation == "render" {
                args.extend([
                    "--page".into(),
                    "1".into(),
                    "--dpi".into(),
                    "72".into(),
                    "--out".into(),
                    work.join(format!("{format}.png")).into_os_string(),
                ]);
            }
        }
    }
    Ok(args)
}

fn check_output(
    name: &str,
    output: &ProcessResult,
    version: &str,
    work: &Path,
    baselines: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    require(
        output.termination.is_none(),
        "SMOKE_PROCESS_LIMIT",
        "Smoke command exceeded its execution budget",
    )?;
    require(
        output.returncode == if name == "typed_error" { 10 } else { 0 },
        "SMOKE_EXIT_CODE",
        "Smoke command returned an unexpected exit code",
    )?;
    match name {
        "version" => require(
            text(&output.stdout)?
                .trim()
                .starts_with(&format!("docsight {version}"))
                && output.stderr.is_empty(),
            "SMOKE_VERSION_MISMATCH",
            "Executable version differs from its manifest",
        ),
        "capabilities" => {
            let result = agent_result(output)?;
            let commands = array(field(&result, "commands")?)?;
            require(
                ["inspect", "render", "diff", "find", "completions"]
                    .iter()
                    .all(|name| {
                        commands.iter().any(|command| {
                            command.get("name").and_then(Value::as_str) == Some(*name)
                        })
                    }),
                "SMOKE_CAPABILITIES",
                "Capabilities omitted required commands",
            )
        }
        "identical_diff" => require(
            agent_result(output)?
                .pointer("/summary/semantic_changes")
                .and_then(Value::as_u64)
                == Some(0),
            "SMOKE_IDENTICAL_DIFF",
            "Identical documents produced semantic changes",
        ),
        "sandbox_inspect" => require(
            agent_result(output)?.get("format").and_then(Value::as_str) == Some("docx"),
            "SMOKE_SANDBOX",
            "Sandboxed inspection did not recognize the document",
        ),
        "typed_error" => require(
            output.stdout.is_empty()
                && parse_json(&output.stderr)?
                    .get("code")
                    .and_then(Value::as_str)
                    == Some("UNSUPPORTED_FORMAT"),
            "SMOKE_ERROR_CONTRACT",
            "Invalid input did not preserve typed errors and clean stdout",
        ),
        name if name.starts_with("completion_") => require(
            text(&output.stdout)?
                .to_ascii_lowercase()
                .contains("docsight")
                && output.stderr.is_empty(),
            "SMOKE_COMPLETION",
            "Completion output is missing or contaminated",
        ),
        name => {
            let (format, operation) = name
                .split_once('_')
                .ok_or_else(|| ToolError::new("UNKNOWN_SMOKE_CHECK", "Unknown smoke check"))?;
            match operation {
                "inspect" => {
                    let result = agent_result(output)?;
                    require(
                        result.get("format").and_then(Value::as_str) == Some(format)
                            && result
                                .get("pages")
                                .and_then(Value::as_u64)
                                .is_some_and(|pages| pages > 0),
                        "SMOKE_INSPECT",
                        "Inspection did not recognize the sample",
                    )?;
                    baselines.insert(format.into(), output.stdout.clone());
                    Ok(())
                }
                "determinism" => require(
                    baselines.get(format) == Some(&output.stdout),
                    "SMOKE_NONDETERMINISTIC",
                    "Repeated inspection changed output bytes",
                ),
                "text" => require(
                    agent_result(output)?
                        .get("blocks")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks.iter().any(|block| {
                                block
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| !text.is_empty())
                            })
                        }),
                    "SMOKE_EMPTY_TEXT",
                    "Text extraction returned no sample content",
                ),
                "render" => {
                    agent_result(output)?;
                    validate_png(&work.join(format!("{format}.png")))
                }
                _ => Err(ToolError::new("UNKNOWN_SMOKE_CHECK", "Unknown smoke check")),
            }
        }
    }
}

pub fn run_checks<R: Runner>(
    binary: &Path,
    package: &Path,
    work: &Path,
    version: &str,
    runner: &mut R,
) -> Result<Vec<Check>> {
    let environment = crate::tooling::process::isolated_environment()?;
    write_new(
        &work.join("invalid.docx"),
        b"not a supported document",
        false,
    )?;
    let mut baselines = BTreeMap::new();
    let mut checks = Vec::new();
    for name in SMOKE_CHECKS {
        let args = arguments(name, binary, package, work)?;
        let mut elapsed_ms = 0;
        let result = runner
            .run(
                &args,
                work,
                &ProcessLimits {
                    timeout: Duration::from_secs(45),
                    output_bytes: 8_388_608,
                },
                Some(&environment),
            )
            .and_then(|result| {
                elapsed_ms = result.elapsed_ms;
                check_output(name, &result, version, work, &mut baselines)
            });
        checks.push(Check {
            name: name.into(),
            passed: result.is_ok(),
            error_code: result.err().map(|error| error.code.into()),
            elapsed_ms,
        });
    }
    Ok(checks)
}

pub fn smoke_archive_with<R: Runner>(archive: &Path, runner: &mut R) -> Result<SmokeReceipt> {
    let manifest = verify_archive(archive)?;
    require(
        manifest.target == native_target()?,
        "SMOKE_HOST_MISMATCH",
        "Archive must run on its native target",
    )?;
    let archive_hash = sha256_file(archive)?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let (binary, manifest) = extract_verified(archive, &root.join("package"))?;
    let work = root.join("work");
    fs::create_dir(&work)?;
    let checks = run_checks(
        &binary,
        &root.join("package"),
        &work,
        &manifest.version,
        runner,
    )?;
    require(
        sha256_file(archive)? == archive_hash,
        "ARCHIVE_CHANGED",
        "Archive changed during smoke execution",
    )?;
    Ok(SmokeReceipt {
        schema: "docsight.release-smoke/v1".into(),
        version: manifest.version,
        target: manifest.target,
        revision: manifest.revision,
        archive_sha256: archive_hash,
        passed: checks.len() == SMOKE_CHECKS.len() && checks.iter().all(|check| check.passed),
        checks,
    })
}

pub fn smoke_archive(archive: &Path) -> Result<SmokeReceipt> {
    smoke_archive_with(archive, &mut NativeRunner)
}
