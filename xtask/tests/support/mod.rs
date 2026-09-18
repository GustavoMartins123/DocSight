use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use xtask::release::{self, DOCUMENTS, EXAMPLES, SMOKE_CHECKS, TARGETS};
use xtask::tooling::common::{self, json_bytes, sha256_file, workspace_version};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Runner};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub struct Fixture {
    pub root: PathBuf,
    _directory: TempDir,
}
impl Fixture {
    pub fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        for document in DOCUMENTS {
            fs::write(
                root.join(document),
                "Synthetic maintenance unit-test resource. Public code: `UNSUPPORTED_FORMAT`.\n",
            )?;
        }
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.96.0\"\n",
        )?;
        fs::create_dir_all(root.join("fixtures/validation"))?;
        for example in EXAMPLES {
            fs::write(root.join("fixtures/validation").join(example), example)?;
        }
        fs::create_dir_all(root.join("schemas/v2"))?;
        fs::create_dir_all(root.join("schemas/ir/v1"))?;
        fs::write(root.join("schemas/v2/agent-envelope.json"), b"{}\n")?;
        fs::write(root.join("schemas/ir/v1/document-ir.json"), b"{}\n")?;
        fs::write(
            root.join("NOTICES"),
            "Synthetic unit-test notices; not product license evidence.\n",
        )?;
        fs::create_dir(root.join("release"))?;
        let entries: Vec<Value> = TARGETS
            .iter()
            .map(|target| {
                let binary = if target.contains("windows") {
                    "docsight.exe"
                } else {
                    "docsight"
                };
                json!({"target": target, "runner": "test-runner", "binary": binary})
            })
            .collect();
        let matrix = json!({ "include": entries });
        save(&root.join("release/targets.json"), &matrix)?;
        Ok(Self {
            root,
            _directory: directory,
        })
    }
    pub fn package(&self, target: &str, destination: &str) -> TestResult<PathBuf> {
        let bin = self.root.join(format!("test-binary-{target}"));
        let worker = self.root.join(format!("test-worker-{target}"));
        let bytes = executable_header(target);
        fs::write(&bin, &bytes)?;
        fs::write(&worker, &bytes)?;
        Ok(release::archive::make_package(
            &self.root,
            &bin,
            &worker,
            target,
            REVISION,
            &self.root.join("NOTICES"),
            &self.root.join(destination),
        )?)
    }
    pub fn receipt(&self, path: &Path) -> TestResult<release::SmokeReceipt> {
        let manifest = release::archive::verify_archive(path)?;
        let receipt = release::SmokeReceipt {
            schema: "docsight.release-smoke/v1".into(),
            version: manifest.version,
            target: manifest.target,
            revision: manifest.revision,
            archive_sha256: sha256_file(path)?,
            passed: true,
            checks: SMOKE_CHECKS
                .iter()
                .map(|name| release::Check {
                    name: (*name).into(),
                    passed: true,
                    error_code: None,
                    elapsed_ms: 1,
                })
                .collect(),
        };
        let parent = path.parent().ok_or("archive parent")?;
        save(
            &parent.join(format!("smoke-{}.json", receipt.target)),
            &receipt,
        )?;
        Ok(receipt)
    }
}

pub fn save(path: &Path, value: &impl serde::Serialize) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, json_bytes(value)?)?;
    Ok(())
}

pub fn executable_header(target: &str) -> Vec<u8> {
    let mut bytes = vec![0; 512];
    if target.contains("linux") {
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(
            &(if target.starts_with("aarch64") {
                183u16
            } else {
                62u16
            })
            .to_le_bytes(),
        );
    } else if target.contains("apple") {
        bytes[..4].copy_from_slice(&0xFEED_FACFu32.to_le_bytes());
        bytes[4..8].copy_from_slice(
            &(if target.starts_with("aarch64") {
                0x0100_000Cu32
            } else {
                0x0100_0007u32
            })
            .to_le_bytes(),
        );
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    } else {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[150..152].copy_from_slice(&2u16.to_le_bytes());
        bytes[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
    }
    bytes
}

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}
pub fn rewrite_archive(path: &Path, edit: impl FnOnce(&mut Vec<Entry>)) -> TestResult {
    let bytes = fs::read(path)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_owned();
        let mode = file.unix_mode().ok_or("unix mode")?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        entries.push(Entry { name, bytes, mode });
    }
    edit(&mut entries);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        writer.start_file(
            entry.name,
            SimpleFileOptions::default().unix_permissions(entry.mode),
        )?;
        writer.write_all(&entry.bytes)?;
    }
    fs::write(path, writer.finish()?.into_inner())?;
    Ok(())
}

pub fn outcome(code: i64, stdout: Vec<u8>, stderr: Vec<u8>) -> ProcessResult {
    ProcessResult {
        returncode: code,
        stdout,
        stderr,
        elapsed_ms: 2,
        termination: None,
    }
}
pub fn agent(value: Value) -> common::Result<ProcessResult> {
    Ok(outcome(
        0,
        json_bytes(&json!({"schema":"docsight.agent/v2","result":value,"warnings":[]}))?,
        Vec::new(),
    ))
}
pub fn version() -> ProcessResult {
    outcome(
        0,
        format!("docsight {}\n", workspace_version()).into_bytes(),
        Vec::new(),
    )
}
pub fn small_png() -> TestResult<Vec<u8>> {
    Ok(docsight_core::encode_png(1, 1, &[20, 40, 60])?)
}

pub struct Callback<F>(pub F);
impl<F> Runner for Callback<F>
where
    F: FnMut(
        &[OsString],
        &Path,
        &ProcessLimits,
        Option<&BTreeMap<OsString, OsString>>,
    ) -> common::Result<ProcessResult>,
{
    fn run(
        &mut self,
        arguments: &[OsString],
        cwd: &Path,
        limits: &ProcessLimits,
        environment: Option<&BTreeMap<OsString, OsString>>,
    ) -> common::Result<ProcessResult> {
        (self.0)(arguments, cwd, limits, environment)
    }
}

pub fn synthetic_engine(
    arguments: &[OsString],
    _: &Path,
    _: &ProcessLimits,
    environment: Option<&BTreeMap<OsString, OsString>>,
) -> common::Result<ProcessResult> {
    common::require(
        environment.is_some_and(|env| !env.contains_key(&OsString::from("GITHUB_TOKEN"))),
        "TEST_ENV",
        "An isolated environment is required",
    )?;
    let contains = |name: &str| arguments.iter().any(|arg| arg == name);
    if contains("--version") {
        return Ok(version());
    }
    if contains("completions") {
        return Ok(outcome(
            0,
            b"docsight completion output".to_vec(),
            Vec::new(),
        ));
    }
    if contains("--json-errors") {
        return Ok(outcome(
            10,
            Vec::new(),
            json_bytes(&json!({"code":"UNSUPPORTED_FORMAT"}))?,
        ));
    }
    if contains("capabilities") {
        return agent(
            json!({"commands":[{"name":"inspect"},{"name":"render"},{"name":"diff"},{"name":"find"},{"name":"completions"}]}),
        );
    }
    if contains("render") {
        let index = arguments
            .iter()
            .position(|arg| arg == "--out")
            .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output"))?;
        let path = arguments
            .get(index + 1)
            .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output path"))?;
        let png = docsight_core::encode_png(1, 1, &[20, 40, 60])
            .map_err(|_| common::ToolError::new("TEST_PNG", "Cannot encode test PNG"))?;
        fs::write(path, png)?;
        return agent(json!({"rendered":true}));
    }
    if contains("diff") {
        return agent(json!({"summary":{"semantic_changes":0}}));
    }
    if contains("text") {
        return agent(json!({"blocks":[{"text":"synthetic unit-test content"}]}));
    }
    let format = if arguments
        .iter()
        .any(|value| value.to_string_lossy().ends_with(".pdf"))
    {
        "pdf"
    } else {
        "docx"
    };
    agent(json!({"format": format, "pages": 1}))
}

pub fn taxonomy() -> xtask::quality::Taxonomy {
    match xtask::quality::load_taxonomy(&common::workspace_root()) {
        Ok(taxonomy) => taxonomy,
        Err(_) => unreachable!("the repository taxonomy is validated by its own test"),
    }
}

pub fn one_case(file: &str, data: &[u8], operation: &str) -> xtask::corpus::Manifest {
    xtask::corpus::Manifest {
        schema: "docsight.corpus/v2".into(),
        cases: vec![xtask::corpus::Case {
            id: "unit-case".into(),
            file: file.into(),
            sha256: common::digest(data),
            origin: "synthetic".into(),
            format: "docx".into(),
            class: "docx-structured".into(),
            operation: operation.into(),
            reference: None,
            expected: xtask::corpus::Expectation {
                exit_code: 0,
                diagnostic_codes: Vec::new(),
                pointer_equals: BTreeMap::new(),
                repeat: 2,
            },
        }],
    }
}

pub fn code<T>(result: common::Result<T>) -> Option<&'static str> {
    result.err().map(|error| error.code)
}
