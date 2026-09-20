use crate::tooling::common::{
    Result, ToolError, contained_file, decode, digest, no_symlinks, parse_json, read_bytes,
    require, sha256_file,
};
use crate::tooling::process::{ProcessLimits, Runner, isolated_environment};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SCENARIO_SCHEMA: &str = "docsight.task-scenario/v1";
pub const RECEIPT_SCHEMA: &str = "docsight.task-receipt/v1";
const MAX_SCENARIO_BYTES: u64 = 1_048_576;

const ALLOWLIST: &[&str] = &[
    "inspect",
    "outline",
    "text",
    "tables",
    "table",
    "page",
    "images",
    "links",
    "evidence",
    "coverage",
    "hit",
    "query",
    "find",
    "overview",
    "focus",
    "peek",
    "context",
    "resolve",
    "diff",
    "render",
    "crop",
    "fingerprint",
];

#[derive(Debug, Deserialize)]
struct Documents {
    primary: String,
    reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Budgets {
    max_invocations: u64,
    max_stdout_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct Step {
    id: String,
    args: Vec<String>,
    expect_exit: i64,
    #[serde(default)]
    expect_stderr_code: Option<String>,
    #[serde(default)]
    require: Vec<String>,
    #[serde(default)]
    values: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Scenario {
    schema: String,
    id: String,
    title: String,
    documents: Documents,
    budgets: Budgets,
    steps: Vec<Step>,
}

#[derive(Debug, Serialize)]
pub struct StepReceipt {
    pub id: String,
    pub exit_code: i64,
    pub stdout_sha256: String,
    pub stderr_code: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct BudgetUse {
    pub invocations: u64,
    pub stdout_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct TaskReceipt {
    pub schema: String,
    pub scenario_id: String,
    pub scenario_sha256: String,
    pub engine_sha256: String,
    pub passed: bool,
    pub steps: Vec<StepReceipt>,
    pub budgets: BudgetUse,
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
        })
}

fn validate_shape(scenario: &Scenario) -> Result<()> {
    require(
        scenario.schema == SCENARIO_SCHEMA,
        "TASK_SCHEMA",
        "Task scenario uses an unknown schema",
    )?;
    require(
        is_slug(&scenario.id),
        "TASK_SCHEMA",
        "Task scenario id is invalid",
    )?;
    require(
        !scenario.title.is_empty() && scenario.title.len() <= 160,
        "TASK_SCHEMA",
        "Task scenario title is invalid",
    )?;
    require(
        is_file_name(&scenario.documents.primary),
        "TASK_SCHEMA",
        "Task primary document is invalid",
    )?;
    require(
        scenario
            .documents
            .reference
            .as_ref()
            .is_none_or(|name| is_file_name(name)),
        "TASK_SCHEMA",
        "Task reference document is invalid",
    )?;
    require(
        scenario.budgets.max_invocations >= 1 && scenario.budgets.max_invocations <= 16,
        "TASK_BUDGET",
        "Task invocation budget is outside its permitted range",
    )?;
    require(
        scenario.budgets.max_stdout_bytes >= 1 && scenario.budgets.max_stdout_bytes <= 16_777_216,
        "TASK_BUDGET",
        "Task byte budget is outside its permitted range",
    )?;
    require(
        !scenario.steps.is_empty() && scenario.steps.len() <= 16,
        "TASK_SCHEMA",
        "Task step count is outside its permitted range",
    )?;
    for step in &scenario.steps {
        require(is_slug(&step.id), "TASK_SCHEMA", "Task step id is invalid")?;
        require(
            !step.args.is_empty(),
            "TASK_SCHEMA",
            "Task step args are empty",
        )?;
        require(
            step.args.len() <= 32,
            "TASK_SCHEMA",
            "Task step declares too many arguments",
        )?;
        require(
            ALLOWLIST.contains(&step.args[0].as_str()),
            "TASK_COMMAND",
            "Task step names an unsupported operation",
        )?;
        for pointer in step.require.iter().chain(step.values.keys()) {
            require(
                pointer.starts_with('/'),
                "TASK_SCHEMA",
                "Task pointer must start with '/'",
            )?;
        }
        require(
            step.expect_exit == 0 || step.expect_stderr_code.is_some(),
            "TASK_SCHEMA",
            "Task failing step must declare its typed stderr code",
        )?;
    }
    Ok(())
}

fn resolve_pointer<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for token in pointer.split('/').skip(1) {
        let token = token.replace("~1", "/").replace("~0", "~");
        match current {
            serde_json::Value::Array(items) => {
                current = items.get(token.parse::<usize>().ok()?)?;
            }
            serde_json::Value::Object(map) => {
                current = map.get(&token)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn scalar_text(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        serde_json::Value::Bool(flag) => Ok(flag.to_string()),
        serde_json::Value::Null => Ok("null".to_owned()),
        _ => Err(ToolError::new(
            "TASK_CHAIN",
            "Chained task value must be a scalar",
        )),
    }
}

fn task_environment() -> Result<BTreeMap<OsString, OsString>> {
    let mut environment = isolated_environment()?;
    for name in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.into(), value);
        }
    }
    Ok(environment)
}

fn display_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| ToolError::new("TASK_PATH", "Task scratch path is not UTF-8"))?;
    #[cfg(windows)]
    {
        let bytes = text.as_bytes();
        if bytes.len() > 6
            && text.starts_with("\\\\?\\")
            && bytes[4].is_ascii_alphabetic()
            && bytes[5] == b':'
        {
            return Ok(text[4..].to_owned());
        }
    }
    Ok(text.to_owned())
}

fn substitute(
    template: &str,
    primary: &Path,
    reference: Option<&Path>,
    scratch: &Path,
    outputs: &BTreeMap<String, serde_json::Value>,
) -> Result<OsString> {
    let primary_text = primary
        .to_str()
        .ok_or_else(|| ToolError::new("TASK_PATH", "Task document path is not UTF-8"))?;
    let scratch_text = display_path(scratch)?;
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest
            .find('}')
            .ok_or_else(|| ToolError::new("TASK_TEMPLATE", "Task placeholder is unclosed"))?;
        if end < start {
            return Err(ToolError::new(
                "TASK_TEMPLATE",
                "Task placeholder is unclosed",
            ));
        }
        rendered.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        if name == "primary" {
            rendered.push_str(primary_text);
        } else if name == "reference" {
            rendered.push_str(reference.and_then(Path::to_str).ok_or_else(|| {
                ToolError::new("TASK_TEMPLATE", "Task scenario has no reference document")
            })?);
        } else if name == "scratch" {
            rendered.push_str(scratch_text.as_str());
        } else if let Some((step, pointer)) = name.split_once('.')
            && pointer.starts_with('/')
        {
            let output = outputs.get(step).ok_or_else(|| {
                ToolError::new("TASK_CHAIN", "Task step references an unknown step")
            })?;
            let value = resolve_pointer(output, pointer).ok_or_else(|| {
                ToolError::new("TASK_CHAIN", "Task step reference does not resolve")
            })?;
            rendered.push_str(&scalar_text(value)?);
        } else {
            return Err(ToolError::new(
                "TASK_TEMPLATE",
                "Task placeholder is unknown",
            ));
        }
        rest = &rest[end + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered.into())
}

fn verify_artifact(step: &Step, body: &serde_json::Value, scratch: &Path) -> Result<()> {
    let mut artifact: Option<&str> = None;
    for (index, arg) in step.args.iter().enumerate() {
        if arg == "--out" {
            artifact = step.args.get(index + 1).map(String::as_str);
            break;
        }
    }
    let Some(template) = artifact else {
        return Ok(());
    };
    require(
        template.starts_with("{scratch}/"),
        "TASK_ARTIFACT",
        "Task artifact must stay inside its scratch directory",
    )?;
    let file_name = template
        .rsplit('/')
        .next()
        .ok_or_else(|| ToolError::new("TASK_ARTIFACT", "Task artifact misses its file name"))?;
    require(
        is_file_name(file_name),
        "TASK_ARTIFACT",
        "Task artifact name is invalid",
    )?;
    let path = scratch.join(file_name);
    let bytes = fs::read(&path)
        .map_err(|_| ToolError::new("TASK_ARTIFACT", "Task artifact was not produced"))?;
    if let Some(expected) = body
        .pointer("/result/output_sha256")
        .and_then(|value| value.as_str())
    {
        require(
            sha256_file(&path)? == expected,
            "TASK_ARTIFACT",
            "Task artifact digest differs from its result",
        )?;
    }
    if let Some(expected) = body
        .pointer("/result/output_bytes")
        .and_then(|value| value.as_u64())
    {
        require(
            expected == bytes.len() as u64,
            "TASK_ARTIFACT",
            "Task artifact size differs from its result",
        )?;
    }
    Ok(())
}

fn check_engine(engine: &Path) -> Result<PathBuf> {
    no_symlinks(engine)?;
    require(
        fs::metadata(engine).is_ok_and(|metadata| metadata.is_file()),
        "TASK_ENGINE",
        "Task engine must be a regular file",
    )?;
    let absolute = engine
        .canonicalize()
        .map_err(|_| ToolError::new("TASK_ENGINE", "Task engine path is unavailable"))?;
    let worker = absolute.with_file_name(if cfg!(windows) {
        "docsight-worker.exe"
    } else {
        "docsight-worker"
    });
    no_symlinks(&worker)?;
    require(
        fs::metadata(&worker).is_ok_and(|metadata| metadata.is_file()),
        "TASK_ENGINE",
        "Task engine requires its worker next to it for sandbox isolation",
    )?;
    Ok(absolute)
}

pub struct RunOptions<'a> {
    pub scenario: &'a Path,
    pub root: &'a Path,
    pub engine: &'a Path,
}

pub fn run_with<R: Runner>(options: &RunOptions<'_>, runner: &mut R) -> Result<TaskReceipt> {
    no_symlinks(options.scenario)?;
    let manifest_bytes = read_bytes(options.scenario, MAX_SCENARIO_BYTES)?;
    let parsed = decode(parse_json(&manifest_bytes)?)?;
    let scenario: Scenario = validate_scenario(parsed, options.root)?;
    run_validated(&scenario, &manifest_bytes, options, runner)
}

fn validate_scenario(scenario: Scenario, root: &Path) -> Result<Scenario> {
    validate_shape(&scenario)?;
    contained_file(root, &scenario.documents.primary)?;
    if let Some(reference) = &scenario.documents.reference {
        contained_file(root, reference)?;
    }
    Ok(scenario)
}

fn run_validated<R: Runner>(
    scenario: &Scenario,
    manifest_bytes: &[u8],
    options: &RunOptions<'_>,
    runner: &mut R,
) -> Result<TaskReceipt> {
    let engine = check_engine(options.engine)?;
    let engine_sha256 = sha256_file(&engine)?;
    let primary = contained_file(options.root, &scenario.documents.primary)?;
    let reference = scenario
        .documents
        .reference
        .as_ref()
        .map(|name| contained_file(options.root, name))
        .transpose()?;
    let environment = task_environment()?;
    let scratch = tempfile::tempdir()
        .map_err(|_| ToolError::new("TASK_SCRATCH", "Task scratch directory is unavailable"))?;
    let scratch_root = scratch
        .path()
        .canonicalize()
        .map_err(|_| ToolError::new("TASK_SCRATCH", "Task scratch directory is unavailable"))?;
    let mut outputs: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut steps = Vec::with_capacity(scenario.steps.len());
    let mut invocations: u64 = 0;
    let mut stdout_bytes: u64 = 0;
    for step in &scenario.steps {
        let mut args: Vec<OsString> = vec![
            engine.as_os_str().to_owned(),
            "--agent".into(),
            "--sandbox".into(),
        ];
        for template in &step.args {
            args.push(substitute(
                template,
                &primary,
                reference.as_deref(),
                &scratch_root,
                &outputs,
            )?);
        }
        let result = runner.run(
            &args,
            &scratch_root,
            &ProcessLimits {
                timeout: Duration::from_secs(45),
                output_bytes: 8_388_608,
            },
            Some(&environment),
        )?;
        invocations = invocations
            .checked_add(1)
            .ok_or_else(|| ToolError::new("TASK_BUDGET", "Task invocation count overflow"))?;
        stdout_bytes = stdout_bytes
            .checked_add(result.stdout.len() as u64)
            .ok_or_else(|| ToolError::new("TASK_BUDGET", "Task byte count overflow"))?;
        require(
            invocations <= scenario.budgets.max_invocations,
            "TASK_BUDGET",
            "Task exceeds its invocation budget",
        )?;
        require(
            stdout_bytes <= scenario.budgets.max_stdout_bytes,
            "TASK_BUDGET",
            "Task exceeds its byte budget",
        )?;
        let exit = result.returncode;
        require(
            exit == step.expect_exit,
            "TASK_RESULT",
            "Task step exit code differs from its scenario",
        )?;
        let mut stderr_code = None;
        if step.expect_exit == 0 {
            require(
                result.termination.is_none() && result.stderr.is_empty(),
                "TASK_RESULT",
                "Task step must keep diagnostics out of stderr on success",
            )?;
            let body: serde_json::Value = decode(parse_json(&result.stdout)?)?;
            for pointer in &step.require {
                require(
                    resolve_pointer(&body, pointer).is_some(),
                    "TASK_RESULT",
                    "Task step result misses required evidence",
                )?;
            }
            for (pointer, expected) in &step.values {
                require(
                    resolve_pointer(&body, pointer) == Some(expected),
                    "TASK_RESULT",
                    "Task step result differs from its scenario",
                )?;
            }
            verify_artifact(step, &body, &scratch_root)?;
            outputs.insert(step.id.clone(), body);
        } else {
            require(
                result.stdout.is_empty(),
                "TASK_RESULT",
                "Task failing step must not emit result bytes",
            )?;
            let envelope: serde_json::Value = decode(parse_json(&result.stderr)?)?;
            let code = envelope
                .pointer("/error/code")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    ToolError::new("TASK_RESULT", "Task error envelope misses its code")
                })?;
            require(
                step.expect_stderr_code.as_deref() == Some(code),
                "TASK_RESULT",
                "Task step error code differs from its scenario",
            )?;
            stderr_code = Some(code.to_owned());
        }
        steps.push(StepReceipt {
            id: step.id.clone(),
            exit_code: exit,
            stdout_sha256: digest(&result.stdout),
            stderr_code,
            elapsed_ms: result.elapsed_ms,
        });
    }
    require(
        sha256_file(options.scenario)? == digest(manifest_bytes),
        "TASK_EVIDENCE_CHANGED",
        "Task scenario changed during execution",
    )?;
    Ok(TaskReceipt {
        schema: RECEIPT_SCHEMA.into(),
        scenario_id: scenario.id.clone(),
        scenario_sha256: digest(manifest_bytes),
        engine_sha256,
        passed: true,
        steps,
        budgets: BudgetUse {
            invocations,
            stdout_bytes,
        },
    })
}

pub fn run(options: &RunOptions<'_>) -> Result<TaskReceipt> {
    run_with(options, &mut crate::tooling::process::NativeRunner)
}
