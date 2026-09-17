mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::encrypted_pdf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/validation")
        .join(name)
}

fn run(prefix: &[&str], arguments: &[String]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(docsight().args(prefix).args(arguments).output()?)
}

fn cache_prefix(directory: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(vec![
        "--cache-dir".to_owned(),
        directory.to_str().ok_or("cache path")?.to_owned(),
    ])
}

fn with_prefix(prefix: &[String], extra: &[&str]) -> Vec<String> {
    let mut arguments: Vec<String> = extra.iter().map(|value| (*value).to_owned()).collect();
    arguments.extend(prefix.iter().cloned());
    arguments
}

fn agent_json(arguments: &[&str]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = docsight().arg("--agent").args(arguments).output()?;
    assert!(
        output.status.success(),
        "{arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn first_id(value: &serde_json::Value, root: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(value["result"][root][0]["id"]
        .as_str()
        .ok_or("fixture object id")?
        .to_owned())
}

fn cache_json(
    directory: &Path,
    action: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = docsight()
        .arg("--agent")
        .arg("--cache-dir")
        .arg(directory)
        .args(["cache", action])
        .output()?;
    assert!(
        output.status.success(),
        "cache {action} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn command_matrix(document: &Path) -> Result<Vec<Vec<String>>, Box<dyn std::error::Error>> {
    let path = document.to_str().ok_or("fixture path")?.to_owned();
    let text = agent_json(&["text", &path])?;
    let block = first_id(&text, "blocks")?;
    let phrase: String = text["result"]["blocks"][0]["text"]
        .as_str()
        .ok_or("fixture block text")?
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let tables = agent_json(&["tables", &path])?;
    let table = first_id(&tables, "tables")?;
    let commands: Vec<Vec<&str>> = vec![
        vec!["inspect", &path],
        vec!["outline", &path],
        vec!["text", &path],
        vec!["tables", &path],
        vec!["table", &path, &table, "--format", "json"],
        vec!["page", &path, "1"],
        vec!["images", &path],
        vec!["links", &path],
        vec!["evidence", &path, &block],
        vec!["coverage", &path],
        vec!["hit", &path, "--page", "1", "--point", "100,80"],
        vec!["query", &path, "heading[level<=2]"],
        vec!["find", &path, "a"],
        vec!["overview", &path],
        vec!["focus", &path, &block],
        vec!["peek", &path, "--page", "1"],
        vec!["context", &path, &block],
        vec!["resolve", &path, "--text", &phrase],
    ];
    Ok(commands
        .into_iter()
        .map(|command| command.into_iter().map(str::to_owned).collect())
        .collect())
}

fn assert_same_output(label: &str, expected: &Output, actual: &Output) {
    assert_eq!(expected.status.code(), actual.status.code(), "{label} exit");
    assert!(
        expected.stdout == actual.stdout,
        "{label} stdout differs:\n{}\n---\n{}",
        String::from_utf8_lossy(&expected.stdout),
        String::from_utf8_lossy(&actual.stdout)
    );
    assert!(
        expected.stderr == actual.stderr,
        "{label} stderr differs:\n{}\n---\n{}",
        String::from_utf8_lossy(&expected.stderr),
        String::from_utf8_lossy(&actual.stderr)
    );
}

#[test]
fn every_cached_command_is_byte_identical_on_miss_and_hit() -> TestResult {
    let capabilities = agent_json(&["capabilities"])?;
    let declared: Vec<&str> = capabilities["result"]["cache"]["applies_to_commands"]
        .as_array()
        .ok_or("cache capability commands")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();

    for fixture_name in ["sample_tables.docx", "sample_table_ruled.pdf"] {
        let directory = tempfile::tempdir()?;
        let cache = cache_prefix(&directory.path().join("cache"))?;
        let matrix = command_matrix(&fixture(fixture_name))?;
        let covered: Vec<&str> = matrix.iter().map(|command| command[0].as_str()).collect();
        assert_eq!(
            covered, declared,
            "the matrix must cover every cached command"
        );

        for command in &matrix {
            for mode in [&["--agent"][..], &[][..]] {
                let label = format!("{fixture_name} {mode:?} {command:?}");
                let plain = run(mode, command)?;
                assert_eq!(plain.status.code(), Some(0), "{label}");
                let first = run(
                    mode,
                    &with_prefix(
                        &cache,
                        &command.iter().map(String::as_str).collect::<Vec<_>>(),
                    ),
                )?;
                let second = run(
                    mode,
                    &with_prefix(
                        &cache,
                        &command.iter().map(String::as_str).collect::<Vec<_>>(),
                    ),
                )?;
                assert_same_output(&format!("{label} first cached run"), &plain, &first);
                assert_same_output(&format!("{label} cache hit"), &plain, &second);
            }
        }
        let stats = cache_json(&directory.path().join("cache"), "stats")?;
        assert_eq!(stats["result"]["stats"]["entries"], 1, "{fixture_name}");
        assert_eq!(stats["result"]["stats"]["current_engine_entries"], 1);
        assert_eq!(stats["result"]["stats"]["temporary_files"], 0);
    }
    Ok(())
}

#[test]
fn sandboxed_runs_share_the_parent_owned_cache() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document = fixture("sample_features.docx");
    let plain = docsight()
        .args(["--agent", "text"])
        .arg(&document)
        .output()?;
    assert!(plain.status.success());

    let temporary = directory.path().join("tmp");
    std::fs::create_dir_all(&temporary)?;
    let sandboxed = |cache: &Path| -> Result<Output, Box<dyn std::error::Error>> {
        let mut command = docsight();
        for variable in ["TMPDIR", "TMP", "TEMP"] {
            command.env(variable, &temporary);
        }
        Ok(command
            .args(["--agent", "--sandbox", "--cache-dir"])
            .arg(cache)
            .arg("text")
            .arg(&document)
            .output()?)
    };
    let miss = sandboxed(&cache_dir)?;
    assert_same_output("sandbox miss", &plain, &miss);
    assert_eq!(
        cache_json(&cache_dir, "stats")?["result"]["stats"]["entries"],
        1,
        "the parent must commit the worker result"
    );
    let hit = sandboxed(&cache_dir)?;
    assert_same_output("sandbox hit", &plain, &hit);
    let inline_flags = docsight()
        .args(["--agent", "--sandbox"])
        .arg(format!(
            "--cache-dir={}",
            cache_dir.to_str().ok_or("cache path")?
        ))
        .args(["--cache-max-entries=8", "--cache-max-bytes=16mb", "text"])
        .arg(&document)
        .output()?;
    assert_same_output("sandbox hit with inline cache flags", &plain, &inline_flags);
    let unsandboxed_hit = docsight()
        .args(["--agent", "--cache-dir"])
        .arg(&cache_dir)
        .arg("text")
        .arg(&document)
        .output()?;
    assert_same_output("unsandboxed hit on sandbox entry", &plain, &unsandboxed_hit);

    let verify = cache_json(&cache_dir, "verify")?;
    assert_eq!(verify["result"]["verify"]["checked"], 1);
    assert_eq!(verify["result"]["verify"]["valid"], 1);
    assert_eq!(verify["result"]["stats"]["sandbox_worker_entries"], 1);
    assert_eq!(verify["result"]["stats"]["in_process_entries"], 0);
    let leaked = std::fs::read_dir(&temporary)?.count();
    assert_eq!(leaked, 0, "handoff directories must be removed");
    Ok(())
}

#[test]
fn corrupted_entries_are_quarantined_in_direct_and_sandboxed_runs() -> TestResult {
    for sandbox in [false, true] {
        let directory = tempfile::tempdir()?;
        let cache_dir = directory.path().join("cache");
        let document = fixture("sample_headings.docx");
        let plain = docsight()
            .args(["--agent", "outline"])
            .arg(&document)
            .output()?;
        let invoke = |extra: &[&str]| -> Result<Output, Box<dyn std::error::Error>> {
            let mut command = docsight();
            command.args(extra);
            if sandbox {
                command.arg("--sandbox");
            }
            Ok(command
                .arg("--cache-dir")
                .arg(&cache_dir)
                .arg("outline")
                .arg(&document)
                .output()?)
        };
        assert_same_output("populate", &plain, &invoke(&["--agent"])?);

        let entries = cache_dir.join("v1").join("entries");
        let entry = std::fs::read_dir(&entries)?
            .next()
            .ok_or("cache entry")??
            .path();
        let mut bytes = std::fs::read(&entry)?;
        let last = bytes.len() - 2;
        bytes[last] ^= 0x20;
        std::fs::write(&entry, &bytes)?;

        let human = invoke(&[])?;
        assert!(human.status.success());
        let stderr = String::from_utf8(human.stderr)?;
        assert!(
            stderr.starts_with("CACHE_ENTRY_QUARANTINED: ") && stderr.contains("(payload_digest)"),
            "sandbox={sandbox}: {stderr}"
        );
        let human_plain = docsight().arg("outline").arg(&document).output()?;
        assert_eq!(human.stdout, human_plain.stdout);

        let quarantine: Vec<String> = std::fs::read_dir(cache_dir.join("v1").join("quarantine"))?
            .filter_map(Result::ok)
            .map(|item| item.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(quarantine.len(), 1);
        assert!(quarantine[0].ends_with(".payload_digest.dsc"));

        let repaired = invoke(&["--agent"])?;
        assert_same_output("hit after reparse", &plain, &repaired);
        let verify = cache_json(&cache_dir, "verify")?;
        assert_eq!(verify["result"]["verify"]["valid"], 1);
        assert_eq!(verify["result"]["stats"]["quarantined_files"], 1);
    }
    Ok(())
}

#[test]
fn verify_reports_structured_quarantine_records() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document = fixture("sample_semantic.pdf");
    let populate = docsight()
        .args(["--agent", "--cache-dir"])
        .arg(&cache_dir)
        .arg("text")
        .arg(&document)
        .output()?;
    assert!(populate.status.success());
    let entry = std::fs::read_dir(cache_dir.join("v1").join("entries"))?
        .next()
        .ok_or("cache entry")??
        .path();
    std::fs::write(&entry, b"{}\n")?;

    let verify = cache_json(&cache_dir, "verify")?;
    let records = verify["result"]["verify"]["quarantined"]
        .as_array()
        .ok_or("quarantine records")?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["reason"], "invalid_header");
    assert_eq!(verify["result"]["verify"]["checked"], 1);
    assert_eq!(verify["result"]["verify"]["valid"], 0);
    assert_eq!(verify["result"]["stats"]["entries"], 0);

    let human = docsight()
        .arg("--cache-dir")
        .arg(&cache_dir)
        .args(["cache", "prune"])
        .output()?;
    assert!(human.status.success());
    assert!(String::from_utf8(human.stdout)?.contains("removed_quarantined_files  1"));
    let stats = cache_json(&cache_dir, "stats")?;
    assert_eq!(stats["result"]["stats"]["quarantined_files"], 0);
    Ok(())
}

#[test]
fn changing_the_document_or_engine_invalidates_the_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document = directory.path().join("document.docx");
    std::fs::copy(fixture("sample_headings.docx"), &document)?;

    let cached_text = |executable: &Path| -> Result<Output, Box<dyn std::error::Error>> {
        let mut attempts = 0;
        loop {
            let result = Command::new(executable)
                .args(["--agent", "--cache-dir"])
                .arg(&cache_dir)
                .arg("text")
                .arg(&document)
                .output();
            match result {
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 100 =>
                {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return Ok(result?),
            }
        }
    };
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_docsight"));
    let first = cached_text(&engine)?;
    assert!(first.status.success());

    std::fs::copy(fixture("sample_tables.docx"), &document)?;
    let replaced = cached_text(&engine)?;
    let plain = docsight()
        .args(["--agent", "text"])
        .arg(&document)
        .output()?;
    assert_same_output("replaced document", &plain, &replaced);
    assert_ne!(first.stdout, replaced.stdout);
    assert_eq!(
        cache_json(&cache_dir, "stats")?["result"]["stats"]["entries"],
        2
    );

    let other_engine = directory
        .path()
        .join(engine.file_name().ok_or("engine name")?);
    let mut bytes = std::fs::read(&engine)?;
    bytes.extend_from_slice(b"docsight-cache-engine-identity-test");
    std::fs::write(&other_engine, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&other_engine, std::fs::Permissions::from_mode(0o755))?;
    }
    let other = cached_text(&other_engine)?;
    assert_same_output("other engine", &plain, &other);
    let stats = cache_json(&cache_dir, "stats")?;
    assert_eq!(stats["result"]["stats"]["entries"], 3);
    assert_eq!(stats["result"]["stats"]["current_engine_entries"], 2);
    assert_eq!(stats["result"]["stats"]["other_engine_entries"], 1);

    let prune = cache_json(&cache_dir, "prune")?;
    assert_eq!(prune["result"]["prune"]["removed_other_engine_entries"], 1);
    assert_eq!(prune["result"]["stats"]["entries"], 2);
    let clear = cache_json(&cache_dir, "clear")?;
    assert_eq!(clear["result"]["clear"]["removed_entries"], 2);
    assert_eq!(clear["result"]["stats"]["entries"], 0);
    Ok(())
}

#[test]
fn failed_parses_are_not_cached() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let pdf = directory.path().join("protected.pdf");
    std::fs::write(&pdf, encrypted_pdf::build(b"test-only-password"))?;

    let plain = docsight().args(["--agent", "text"]).arg(&pdf).output()?;
    let cached = docsight()
        .args(["--agent", "--cache-dir"])
        .arg(&cache_dir)
        .arg("text")
        .arg(&pdf)
        .output()?;
    assert_eq!(plain.status.code(), Some(12));
    assert_same_output("encrypted", &plain, &cached);
    assert_eq!(
        cache_json(&cache_dir, "stats")?["result"]["stats"]["entries"],
        0
    );
    Ok(())
}

#[test]
fn limits_evict_least_recently_used_entries() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    for name in [
        "sample_headings.docx",
        "sample_tables.docx",
        "sample_features.docx",
    ] {
        let output = docsight()
            .args(["--agent", "--cache-max-entries", "2", "--cache-dir"])
            .arg(&cache_dir)
            .arg("outline")
            .arg(fixture(name))
            .output()?;
        assert!(output.status.success());
    }
    let stats = cache_json(&cache_dir, "stats")?;
    assert_eq!(stats["result"]["stats"]["entries"], 2);

    let tiny = docsight()
        .args(["--agent", "--cache-max-bytes", "64", "--cache-dir"])
        .arg(directory.path().join("tiny"))
        .arg("outline")
        .arg(fixture("sample_headings.docx"))
        .output()?;
    assert!(tiny.status.success());
    assert_eq!(
        cache_json(&directory.path().join("tiny"), "stats")?["result"]["stats"]["entries"],
        0,
        "entries larger than the byte limit are not stored"
    );
    Ok(())
}

#[test]
fn concurrent_processes_converge_on_one_valid_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document = fixture("sample_table_ruled.pdf");
    let plain = docsight()
        .args(["--agent", "tables"])
        .arg(&document)
        .output()?;
    let children = (0..8)
        .map(|index| {
            let mut command = docsight();
            command.arg("--agent");
            if index % 2 == 1 {
                command.arg("--sandbox");
            }
            command
                .arg("--cache-dir")
                .arg(&cache_dir)
                .arg("tables")
                .arg(&document)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        })
        .collect::<Result<Vec<_>, _>>()?;
    for child in children {
        let output = child.wait_with_output()?;
        assert_same_output("concurrent", &plain, &output);
    }
    let verify = cache_json(&cache_dir, "verify")?;
    assert_eq!(verify["result"]["verify"]["checked"], 1);
    assert_eq!(verify["result"]["verify"]["valid"], 1);
    assert_eq!(verify["result"]["stats"]["temporary_files"], 0);
    assert_eq!(verify["result"]["stats"]["quarantined_files"], 0);
    Ok(())
}

#[test]
fn cache_arguments_outside_the_contract_are_rejected() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let cache = cache_dir.to_str().ok_or("cache path")?;
    let document = fixture("sample_headings.docx");
    let document = document.to_str().ok_or("fixture path")?;
    let password = directory.path().join("password.txt");
    std::fs::write(&password, b"secret\n")?;
    let password = password.to_str().ok_or("password path")?;
    let out = directory.path().join("page.png");
    let out = out.to_str().ok_or("output path")?;

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["--cache-max-bytes", "1mb", "text", document],
            "require --cache-dir",
        ),
        (
            vec!["--cache-max-entries", "3", "text", document],
            "require --cache-dir",
        ),
        (
            vec!["cache", "stats"],
            "cache maintenance requires --cache-dir",
        ),
        (
            vec![
                "--cache-dir",
                cache,
                "render",
                document,
                "--page",
                "1",
                "--out",
                out,
            ],
            "applies only to commands that load the document IR",
        ),
        (
            vec!["--cache-dir", cache, "fingerprint", document],
            "applies only to commands that load the document IR",
        ),
        (
            vec![
                "--cache-dir",
                cache,
                "--password-file",
                password,
                "text",
                document,
            ],
            "decrypted document content is never persisted",
        ),
        (
            vec!["--sandbox", "--cache-dir", cache, "cache", "stats"],
            "cannot run with --sandbox",
        ),
        (
            vec!["--cache-dir", cache, "--max-items", "1", "cache", "stats"],
            "machine-output limits",
        ),
        (
            vec![
                "--cache-dir",
                cache,
                "--cache-max-entries",
                "0",
                "text",
                document,
            ],
            "cache limits must be greater than zero",
        ),
    ];
    for (arguments, message) in cases {
        let output = docsight().arg("--agent").args(&arguments).output()?;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
        assert_eq!(error["error"]["code"], "USAGE");
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|text| text.contains(message)),
            "{arguments:?}: {error}"
        );
    }

    for sandbox in [false, true] {
        let mut command = docsight();
        command
            .env("DOCSIGHT_CACHE_HANDOFF_DIR", directory.path())
            .arg("--agent");
        if sandbox {
            command.arg("--sandbox");
        }
        let reserved = command.args(["text", document]).output()?;
        assert_eq!(reserved.status.code(), Some(2), "sandbox={sandbox}");
        assert!(reserved.stdout.is_empty());
        assert!(String::from_utf8(reserved.stderr)?.contains("reserved for sandbox workers"));
    }

    let file_as_cache = directory.path().join("not-a-directory");
    std::fs::write(&file_as_cache, b"")?;
    let output = docsight()
        .args(["--agent", "--cache-dir"])
        .arg(&file_as_cache)
        .args(["text", document])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    Ok(())
}

#[test]
fn cache_reports_match_the_published_schema() -> TestResult {
    let schema: serde_json::Value = serde_json::from_slice(&std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/v2/cache-result.json"),
    )?)?;
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let populate = docsight()
        .args(["--agent", "--cache-dir"])
        .arg(&cache_dir)
        .arg("inspect")
        .arg(fixture("sample_headings.docx"))
        .output()?;
    assert!(populate.status.success());

    for action in ["stats", "verify", "prune", "clear"] {
        let envelope = cache_json(&cache_dir, action)?;
        assert_eq!(envelope["schema"], "docsight.agent/v2");
        let result = envelope["result"].as_object().ok_or("cache result")?;
        assert_eq!(result["action"], action);
        assert_eq!(result["layout"], "docsight.cache/v1");
        let properties = schema["properties"]
            .as_object()
            .ok_or("schema properties")?;
        for key in result.keys() {
            assert!(properties.contains_key(key), "{action}: unexpected {key}");
            let expected: Vec<&str> = properties[key]["required"]
                .as_array()
                .map(|fields| {
                    fields
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(object) = result[key].as_object() {
                let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
                let mut expected = expected;
                actual.sort_unstable();
                expected.sort_unstable();
                assert_eq!(actual, expected, "{action}: {key}");
            }
        }
        for required in schema["required"].as_array().ok_or("schema required")? {
            assert!(result.contains_key(required.as_str().ok_or("required key")?));
        }
        if action != "stats" {
            assert!(result.contains_key(action));
        }

        let ndjson = docsight()
            .args(["--agent", "--ndjson", "--cache-dir"])
            .arg(&cache_dir)
            .args(["cache", action])
            .output()?;
        assert!(ndjson.status.success());
        let records = ndjson
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice)
            .collect::<Result<Vec<serde_json::Value>, _>>()?;
        let types: Vec<&str> = records
            .iter()
            .filter_map(|record| record["type"].as_str())
            .collect();
        assert_eq!(types, ["meta", "cache", "done"]);
    }

    let capabilities = agent_json(&["capabilities"])?;
    let cache = &capabilities["result"]["cache"];
    assert_eq!(cache["flag"], "--cache-dir");
    assert_eq!(cache["agent_default"], false);
    assert_eq!(cache["sandbox_behavior"], "parent_process_owns_cache");
    assert_eq!(cache["password_file_behavior"], "reject");
    assert_eq!(cache["output_identity"], "byte_identical");
    let command = capabilities["result"]["commands"]
        .as_array()
        .ok_or("commands")?
        .iter()
        .find(|command| command["name"] == "cache")
        .ok_or("cache command capability")?;
    assert_eq!(
        command["result_schema"],
        "https://docsight.dev/schemas/v2/cache-result.json"
    );
    Ok(())
}

fn entry_file(cache_dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::fs::read_dir(cache_dir.join("v1").join("entries"))?
        .next()
        .ok_or("cache entry")??
        .path())
}

/// Rewrites the payload of an entry and repairs the header digest, so the entry passes the
/// parent's integrity check and is only rejected when the worker decodes it.
fn forge_entry_payload(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::Digest;
    let bytes = std::fs::read(path)?;
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("entry header")?;
    let mut header: serde_json::Value = serde_json::from_slice(&bytes[..newline])?;
    let mut payload: serde_json::Value = serde_json::from_slice(&bytes[newline + 1..])?;
    payload["sha256"] = serde_json::json!("0".repeat(64));
    let payload = serde_json::to_vec(&payload)?;
    let digest: String = sha2::Sha256::digest(&payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    header["payload_sha256"] = serde_json::json!(digest);
    header["payload_bytes"] = serde_json::json!(payload.len());
    let mut forged = serde_json::to_vec(&header)?;
    forged.push(b'\n');
    forged.extend_from_slice(&payload);
    std::fs::write(path, forged)?;
    Ok(())
}

#[test]
fn an_entry_the_worker_rejects_is_revalidated_by_the_parent() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document = fixture("sample_headings.docx");
    let plain = docsight()
        .args(["--agent", "outline"])
        .arg(&document)
        .output()?;
    assert!(plain.status.success());

    let sandboxed = || -> Result<Output, Box<dyn std::error::Error>> {
        Ok(docsight()
            .args(["--agent", "--sandbox", "--cache-dir"])
            .arg(&cache_dir)
            .arg("outline")
            .arg(&document)
            .output()?)
    };
    assert_same_output("populate", &plain, &sandboxed()?);
    forge_entry_payload(&entry_file(&cache_dir)?)?;

    assert_same_output("forged entry", &plain, &sandboxed()?);
    let verify = cache_json(&cache_dir, "verify")?;
    assert_eq!(verify["result"]["verify"]["valid"], 1);
    assert_eq!(verify["result"]["stats"]["quarantined_files"], 1);
    let quarantined: Vec<String> = std::fs::read_dir(cache_dir.join("v1").join("quarantine"))?
        .filter_map(Result::ok)
        .map(|item| item.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        quarantined[0].ends_with(".document_identity.dsc"),
        "{quarantined:?}"
    );
    assert_same_output("hit after reparse", &plain, &sandboxed()?);
    Ok(())
}

#[test]
fn a_cache_that_cannot_be_written_fails_explicitly() -> TestResult {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let cache_dir = directory.path().join("cache");
        let document = fixture("sample_headings.docx");
        let plain = docsight()
            .args(["--agent", "outline"])
            .arg(&document)
            .output()?;
        let populate = docsight()
            .args(["--agent", "--cache-dir"])
            .arg(&cache_dir)
            .arg("outline")
            .arg(fixture("sample_tables.docx"))
            .output()?;
        assert!(populate.status.success());

        let entries = cache_dir.join("v1").join("entries");
        std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o500))?;
        if std::fs::write(entries.join("probe"), b"").is_ok() {
            return Ok(());
        }
        let blocked = docsight()
            .args(["--agent", "--cache-dir"])
            .arg(&cache_dir)
            .arg("outline")
            .arg(&document)
            .output()?;
        assert_eq!(blocked.status.code(), Some(40));
        assert!(blocked.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&blocked.stderr)?;
        assert_eq!(error["error"]["code"], "IO_ERROR");

        let hit = docsight()
            .args(["--agent", "--cache-dir"])
            .arg(&cache_dir)
            .arg("outline")
            .arg(fixture("sample_tables.docx"))
            .output()?;
        assert!(hit.status.success(), "a stored entry stays readable");
        std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o700))?;
        assert_same_output(
            "after restoring permissions",
            &plain,
            &docsight()
                .args(["--agent", "--cache-dir"])
                .arg(&cache_dir)
                .arg("outline")
                .arg(&document)
                .output()?,
        );
    }
    Ok(())
}

#[test]
fn killing_the_process_never_publishes_a_partial_entry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cache_dir = directory.path().join("cache");
    let document =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Projeto_DOCSIGHT_Especificacao.docx");
    let plain = docsight()
        .args(["--agent", "text"])
        .arg(&document)
        .output()?;
    assert!(plain.status.success());

    for step in 0..40 {
        let mut child = docsight()
            .args(["--agent", "--cache-dir"])
            .arg(&cache_dir)
            .arg("text")
            .arg(&document)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        std::thread::sleep(std::time::Duration::from_millis(step * 5));
        let killed = child.kill().is_ok();
        let output = child.wait_with_output()?;
        if !killed || output.status.success() {
            assert_same_output("uninterrupted run", &plain, &output);
        }
        let verify = cache_json(&cache_dir, "verify")?;
        assert!(
            verify["result"]["verify"]["quarantined"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "step {step}: {verify}"
        );
        assert_same_output(
            "after an interrupted run",
            &plain,
            &docsight()
                .args(["--agent", "--cache-dir"])
                .arg(&cache_dir)
                .arg("text")
                .arg(&document)
                .output()?,
        );
    }
    let stats = cache_json(&cache_dir, "stats")?;
    assert_eq!(stats["result"]["stats"]["entries"], 1);
    assert_eq!(stats["result"]["stats"]["temporary_files"], 0);
    Ok(())
}
