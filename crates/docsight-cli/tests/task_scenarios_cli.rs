use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fail(message: String) -> Box<dyn std::error::Error> {
    message.into()
}

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
    values: BMap,
}

type BMap = BTreeMap<String, Value>;

#[derive(Debug, Deserialize)]
struct Scenario {
    schema: String,
    id: String,
    title: String,
    documents: Documents,
    budgets: Budgets,
    steps: Vec<Step>,
}

fn docsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_docsight"))
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
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

fn validate(scenario: &Scenario) -> TestResult {
    if scenario.schema != "docsight.task-scenario/v1" {
        return Err(fail(format!("unexpected schema {}", scenario.schema)));
    }
    if !is_slug(&scenario.id) {
        return Err(fail(format!("invalid scenario id {}", scenario.id)));
    }
    if scenario.title.is_empty() || scenario.title.len() > 160 {
        return Err(fail(format!("invalid title for {}", scenario.id)));
    }
    if !is_file_name(&scenario.documents.primary) {
        return Err(fail(format!(
            "invalid primary document for {}",
            scenario.id
        )));
    }
    if scenario
        .documents
        .reference
        .as_ref()
        .is_some_and(|name| !is_file_name(name))
    {
        return Err(fail(format!(
            "invalid reference document for {}",
            scenario.id
        )));
    }
    if scenario.budgets.max_invocations == 0 || scenario.budgets.max_invocations > 16 {
        return Err(fail(format!(
            "invalid invocation budget for {}",
            scenario.id
        )));
    }
    if scenario.budgets.max_stdout_bytes == 0 || scenario.budgets.max_stdout_bytes > 16_777_216 {
        return Err(fail(format!("invalid byte budget for {}", scenario.id)));
    }
    if scenario.steps.is_empty() || scenario.steps.len() > 16 {
        return Err(fail(format!("invalid step count for {}", scenario.id)));
    }
    for step in &scenario.steps {
        if !is_slug(&step.id) {
            return Err(fail(format!("invalid step id {}", step.id)));
        }
        let command = step
            .args
            .first()
            .ok_or_else(|| fail("empty step args".to_owned()))?;
        if !ALLOWLIST.contains(&command.as_str()) {
            return Err(fail(format!("command {command} is not a task operation")));
        }
        for pointer in step.require.iter().chain(step.values.keys()) {
            if !pointer.starts_with('/') {
                return Err(fail(format!("pointer {pointer} must start with '/'")));
            }
        }
        if step.expect_exit != 0 && step.expect_stderr_code.is_none() {
            return Err(fail(format!(
                "step {} expects a typed stderr code",
                step.id
            )));
        }
    }
    Ok(())
}

fn resolve_pointer<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = value;
    for token in pointer.split('/').skip(1) {
        let token = token.replace("~1", "/").replace("~0", "~");
        match current {
            Value::Array(items) => {
                let index: usize = token.parse().ok()?;
                current = items.get(index)?;
            }
            Value::Object(map) => {
                current = map.get(&token)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn scalar_text(value: &Value) -> TestResult<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Null => Ok("null".to_owned()),
        _ => Err(fail("chained value must be a scalar".to_owned())),
    }
}

fn substitute(
    template: &str,
    primary: &str,
    reference: Option<&str>,
    scratch: &Path,
    outputs: &BTreeMap<String, Value>,
) -> TestResult<String> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest
            .find('}')
            .ok_or_else(|| fail("unclosed placeholder".to_owned()))?;
        if end < start {
            return Err(fail("unclosed placeholder".to_owned()));
        }
        rendered.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        if name == "primary" {
            rendered.push_str(primary);
        } else if name == "reference" {
            rendered.push_str(reference.ok_or_else(|| fail("no reference document".to_owned()))?);
        } else if name == "scratch" {
            rendered.push_str(
                scratch
                    .to_str()
                    .ok_or_else(|| fail("scratch path is not UTF-8".to_owned()))?,
            );
        } else if let Some((step, pointer)) = name.split_once('.')
            && pointer.starts_with('/')
        {
            let output = outputs
                .get(step)
                .ok_or_else(|| fail(format!("unknown step {step}")))?;
            let value = resolve_pointer(output, pointer)
                .ok_or_else(|| fail(format!("unresolved {name}")))?;
            rendered.push_str(&scalar_text(value)?);
        } else {
            return Err(fail(format!("unknown placeholder {name}")));
        }
        rest = &rest[end + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

struct Execution {
    invocations: u64,
    stdout_bytes: u64,
    outputs: BTreeMap<String, Value>,
    bodies: BTreeMap<String, Vec<u8>>,
    scratch: String,
}

fn run_step(
    scenario: &Scenario,
    step: &Step,
    primary: &str,
    reference: Option<&str>,
    scratch: &Path,
    execution: &mut Execution,
) -> TestResult {
    let mut args = Vec::with_capacity(step.args.len());
    for template in &step.args {
        args.push(substitute(
            template,
            primary,
            reference,
            scratch,
            &execution.outputs,
        )?);
    }
    let mut command = docsight();
    command.arg("--agent");
    command.arg("--sandbox");
    command.args(&args);
    let output = command.output()?;
    execution.invocations = execution
        .invocations
        .checked_add(1)
        .ok_or_else(|| fail("invocation count overflow".to_owned()))?;
    execution.stdout_bytes = execution
        .stdout_bytes
        .checked_add(output.stdout.len() as u64)
        .ok_or_else(|| fail("byte count overflow".to_owned()))?;
    if execution.invocations > scenario.budgets.max_invocations {
        return Err(fail(format!(
            "step {} exceeds the invocation budget",
            step.id
        )));
    }
    if execution.stdout_bytes > scenario.budgets.max_stdout_bytes {
        return Err(fail(format!("step {} exceeds the byte budget", step.id)));
    }
    let exit = output
        .status
        .code()
        .ok_or_else(|| fail("missing exit code".to_owned()))? as i64;
    if exit != step.expect_exit {
        return Err(fail(format!(
            "step {} exited {exit}: {}",
            step.id,
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        )));
    }
    if step.expect_exit == 0 {
        if !output.stderr.is_empty() {
            return Err(fail(format!("step {} wrote to stderr", step.id)));
        }
        let body: Value = serde_json::from_slice(&output.stdout)?;
        for pointer in &step.require {
            if resolve_pointer(&body, pointer).is_none() {
                return Err(fail(format!("step {} misses {pointer}", step.id)));
            }
        }
        for (pointer, expected) in &step.values {
            let actual = resolve_pointer(&body, pointer)
                .ok_or_else(|| fail(format!("step {} misses {pointer}", step.id)))?;
            if actual != expected {
                return Err(fail(format!("step {} differs at {pointer}", step.id)));
            }
        }
        verify_artifact(step, &body, scratch)?;
        execution.outputs.insert(step.id.clone(), body);
    } else {
        if !output.stdout.is_empty() {
            return Err(fail(format!("failing step {} wrote to stdout", step.id)));
        }
        let envelope: Value = serde_json::from_slice(&output.stderr)?;
        let code = envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("error envelope misses its code".to_owned()))?;
        let expected = step
            .expect_stderr_code
            .as_ref()
            .ok_or_else(|| fail("missing expected stderr code".to_owned()))?;
        if code != expected {
            return Err(fail(format!("step {} reported {code}", step.id)));
        }
    }
    execution
        .bodies
        .insert(step.id.clone(), output.stdout.clone());
    Ok(())
}

fn verify_artifact(step: &Step, body: &Value, scratch: &Path) -> TestResult {
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
    if !template.starts_with("{scratch}/") {
        return Err(fail(format!("step {} writes outside its scratch", step.id)));
    }
    let file_name = template
        .rsplit('/')
        .next()
        .ok_or_else(|| fail("artifact misses its file name".to_owned()))?;
    if !is_file_name(file_name) {
        return Err(fail(format!("step {} names an unsafe artifact", step.id)));
    }
    let path = scratch.join(file_name);
    let bytes = std::fs::read(&path)?;
    if let Some(expected) = body
        .pointer("/result/output_sha256")
        .and_then(Value::as_str)
    {
        let mut digest = Sha256::new();
        digest.update(&bytes);
        let actual = hex(digest.finalize());
        if actual != expected {
            return Err(fail(format!("step {} artifact digest differs", step.id)));
        }
    }
    if let Some(expected) = body.pointer("/result/output_bytes").and_then(Value::as_u64)
        && expected != bytes.len() as u64
    {
        return Err(fail(format!("step {} artifact size differs", step.id)));
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn normalized(body: &[u8], scratch: &str) -> String {
    let text = String::from_utf8_lossy(body);
    let escaped = scratch.replace('\\', "\\\\");
    text.replace(&escaped, "{scratch}")
        .replace(scratch, "{scratch}")
}

fn load_scenario(name: &str) -> TestResult<Scenario> {
    let path = workspace().join("fixtures").join("tasks").join(name);
    let scenario: Scenario = serde_json::from_slice(&std::fs::read(path)?)?;
    validate(&scenario)?;
    Ok(scenario)
}

fn document_path(name: &str) -> TestResult<String> {
    let path = workspace().join("fixtures").join("validation").join(name);
    if !path.is_file() {
        return Err(fail(format!("missing fixture {name}")));
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| fail("fixture path is not UTF-8".to_owned()))
}

fn execute(scenario: &Scenario) -> TestResult<Execution> {
    let primary = document_path(&scenario.documents.primary)?;
    let reference = scenario
        .documents
        .reference
        .as_deref()
        .map(document_path)
        .transpose()?;
    let scratch = tempfile::tempdir()?;
    let scratch_text = scratch
        .path()
        .to_str()
        .ok_or_else(|| fail("scratch path is not UTF-8".to_owned()))?
        .to_owned();
    let mut execution = Execution {
        invocations: 0,
        stdout_bytes: 0,
        outputs: BTreeMap::new(),
        bodies: BTreeMap::new(),
        scratch: scratch_text,
    };
    for step in &scenario.steps {
        run_step(
            scenario,
            step,
            &primary,
            reference.as_deref(),
            scratch.path(),
            &mut execution,
        )?;
    }
    Ok(execution)
}

fn scenario_names() -> TestResult<Vec<String>> {
    let directory = workspace().join("fixtures").join("tasks");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| fail("task file name is not UTF-8".to_owned()))?;
        if name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(fail("no task scenarios".to_owned()));
    }
    Ok(names)
}

#[test]
fn task_scenarios_run_end_to_end_on_budgets() -> TestResult {
    for name in scenario_names()? {
        let scenario = load_scenario(&name)?;
        let first = execute(&scenario)?;
        assert_eq!(first.invocations as usize, scenario.steps.len());
        let second = execute(&scenario)?;
        for step in &scenario.steps {
            let first_body = normalized(
                first
                    .bodies
                    .get(&step.id)
                    .ok_or_else(|| fail("missing first body".to_owned()))?,
                &first.scratch,
            );
            let second_body = normalized(
                second
                    .bodies
                    .get(&step.id)
                    .ok_or_else(|| fail("missing second body".to_owned()))?,
                &second.scratch,
            );
            assert_eq!(
                first_body, second_body,
                "scenario {} step {}",
                scenario.id, step.id
            );
        }
    }
    Ok(())
}

#[test]
fn task_manifests_reject_unknown_commands() {
    let mut scenario = Scenario {
        schema: "docsight.task-scenario/v1".to_owned(),
        id: "reject-command".to_owned(),
        title: "reject".to_owned(),
        documents: Documents {
            primary: "sample_tables.docx".to_owned(),
            reference: None,
        },
        budgets: Budgets {
            max_invocations: 1,
            max_stdout_bytes: 1024,
        },
        steps: vec![Step {
            id: "wipe".to_owned(),
            args: vec!["wipe".to_owned()],
            expect_exit: 0,
            expect_stderr_code: None,
            require: Vec::new(),
            values: BTreeMap::new(),
        }],
    };
    assert!(validate(&scenario).is_err());
    scenario.steps[0].args = vec!["inspect".to_owned(), "{cache}/evil".to_owned()];
    assert!(
        substitute(
            &scenario.steps[0].args[1],
            "primary",
            None,
            Path::new("scratch"),
            &BTreeMap::new(),
        )
        .is_err()
    );
}
