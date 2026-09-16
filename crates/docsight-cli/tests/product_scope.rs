use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_document(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(std::fs::read_to_string(repository_root().join(name))?)
}

fn declared_commands() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .args(["--agent", "capabilities"])
        .output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let commands = value["result"]["commands"]
        .as_array()
        .ok_or("capabilities declares no commands")?
        .iter()
        .filter_map(|command| command["name"].as_str())
        .map(str::to_owned)
        .collect();
    Ok(commands)
}

fn diagnostic_codes_in_sources() -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let mut codes = BTreeSet::new();
    let crates_dir = repository_root().join("crates");
    for entry in std::fs::read_dir(&crates_dir)? {
        collect_codes(&entry?.path().join("src"), &mut codes)?;
    }
    Ok(codes)
}

fn collect_codes(
    directory: &PathBuf,
    codes: &mut BTreeSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !directory.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_codes(&path, codes)?;
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        for literal in source.split('"').skip(1).step_by(2) {
            if is_diagnostic_code(literal) {
                codes.insert(literal.to_owned());
            }
        }
    }
    Ok(())
}

fn is_diagnostic_code(literal: &str) -> bool {
    const PREFIXES: [&str; 5] = ["DOCX_", "PDF_", "DIFF_", "CONTEXT_", "TRACE_"];
    const EXACT: [&str; 7] = [
        "APPROXIMATED_PDF_FONT",
        "INFERRED_SEMANTICS",
        "INFERRED_TABLE",
        "LOW_CONFIDENCE_RECONSTRUCTION",
        "OBJECT_CROP_CLIPPED_TO_PAGE",
        "RENDER_FINGERPRINT_UNAVAILABLE",
        "SPATIAL_GEOMETRY_UNAVAILABLE",
    ];
    if literal == "DOCX_PDF_FORMATS" || literal.starts_with("DOCSIGHT_") {
        return false;
    }
    EXACT.contains(&literal)
        || (PREFIXES.iter().any(|prefix| literal.starts_with(prefix))
            && literal
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
}

#[test]
fn product_scope_declares_the_required_sections() -> Result<(), Box<dyn std::error::Error>> {
    let scope = read_document("PRODUCT_SCOPE.md")?;
    for section in ["## Supported", "## Partial", "## Out of scope"] {
        assert!(
            scope.contains(section),
            "PRODUCT_SCOPE.md is missing the {section} section"
        );
    }
    Ok(())
}

#[test]
fn backlog_separates_v1_from_later_work() -> Result<(), Box<dyn std::error::Error>> {
    let backlog = read_document("BACKLOG.md")?;
    for section in ["## v1", "## post-v1", "## experimental"] {
        assert!(
            backlog.contains(section),
            "BACKLOG.md is missing the {section} section"
        );
    }
    Ok(())
}

#[test]
fn every_declared_command_appears_in_the_product_scope() -> Result<(), Box<dyn std::error::Error>> {
    let scope = read_document("PRODUCT_SCOPE.md")?;
    let missing: Vec<String> = declared_commands()?
        .into_iter()
        .filter(|command| !scope.contains(&format!("`{command}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "commands are declared by capabilities but absent from PRODUCT_SCOPE.md: {missing:?}"
    );
    Ok(())
}

fn cli_subcommands() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_docsight"))
        .arg("--help")
        .output()?;
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout)?;
    let section = help
        .split("Commands:")
        .nth(1)
        .and_then(|rest| rest.split("Options:").next())
        .ok_or("help output has no Commands section")?;
    Ok(section
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_owned)
        .collect())
}

#[test]
fn every_cli_subcommand_is_declared_by_capabilities() -> Result<(), Box<dyn std::error::Error>> {
    let declared = declared_commands()?;
    let subcommands = cli_subcommands()?;
    assert!(
        subcommands.len() >= 20,
        "the help parser found only {} subcommands",
        subcommands.len()
    );
    let undeclared: Vec<&String> = subcommands
        .iter()
        .filter(|name| !declared.contains(name))
        .collect();
    let phantom: Vec<&String> = declared
        .iter()
        .filter(|name| !subcommands.contains(name))
        .collect();
    assert!(
        undeclared.is_empty(),
        "subcommands exist but capabilities does not declare them: {undeclared:?}"
    );
    assert!(
        phantom.is_empty(),
        "capabilities declares commands the CLI does not implement: {phantom:?}"
    );
    Ok(())
}

#[test]
fn no_declared_command_contradicts_the_out_of_scope_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    const FORBIDDEN: [&str; 9] = [
        "ocr", "edit", "write", "serve", "daemon", "convert", "xlsx", "pptx", "ask",
    ];
    let offending: Vec<String> = declared_commands()?
        .into_iter()
        .filter(|command| FORBIDDEN.contains(&command.as_str()))
        .collect();
    assert!(
        offending.is_empty(),
        "commands contradict the out-of-scope boundaries in PRODUCT_SCOPE.md: {offending:?}"
    );
    Ok(())
}

#[test]
fn every_declared_exit_code_appears_in_the_product_scope() -> Result<(), Box<dyn std::error::Error>>
{
    let scope = read_document("PRODUCT_SCOPE.md")?;
    let mut codes: BTreeSet<u8> = docsight_core::ERROR_CATALOG
        .iter()
        .map(|entry| entry.exit_code)
        .collect();
    codes.insert(docsight_core::SUCCESS_EXIT_CODE);
    let missing: Vec<u8> = codes
        .into_iter()
        .filter(|code| !scope.contains(&format!("`{code}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "exit codes are part of the error contract but absent from PRODUCT_SCOPE.md: {missing:?}"
    );
    Ok(())
}

#[test]
fn every_emitted_diagnostic_code_is_documented() -> Result<(), Box<dyn std::error::Error>> {
    let scope = read_document("PRODUCT_SCOPE.md")?;
    let emitted = diagnostic_codes_in_sources()?;
    assert!(
        emitted.len() >= 25 && emitted.contains("DOCX_SECTIONS_COLLAPSED"),
        "the diagnostic code scan found {} codes, which means it stopped matching the sources",
        emitted.len()
    );
    let missing: Vec<String> = emitted
        .into_iter()
        .filter(|code| !scope.contains(code))
        .collect();
    assert!(
        missing.is_empty(),
        "diagnostic codes are emitted by the engine but absent from PRODUCT_SCOPE.md: {missing:?}"
    );
    Ok(())
}

#[test]
fn documented_diagnostic_codes_are_actually_emitted() -> Result<(), Box<dyn std::error::Error>> {
    let scope = read_document("PRODUCT_SCOPE.md")?;
    let emitted = diagnostic_codes_in_sources()?;
    let documented: BTreeSet<String> = scope
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|literal| is_diagnostic_code(literal))
        .map(str::to_owned)
        .collect();
    let phantom: Vec<String> = documented
        .into_iter()
        .filter(|code| !emitted.contains(code))
        .collect();
    assert!(
        phantom.is_empty(),
        "PRODUCT_SCOPE.md documents diagnostic codes the engine never emits: {phantom:?}"
    );
    Ok(())
}
