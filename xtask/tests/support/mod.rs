use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use xtask::release::{self, DOCUMENTS, EXAMPLES, SMOKE_CHECKS, TARGETS, distribution};
use xtask::tooling::common::{self, json_bytes, sha256_file, workspace_version};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Runner};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const TEAM: &str = "ABCDE12345";
pub const SUBJECT: &str = "CN=Example Publisher, O=Example Publisher, L=Sao Paulo, C=BR";
pub const REPOSITORY: &str = "GustavoMartins123/DocSight";
pub const SIGNER: &str = "GustavoMartins123/DocSight/.github/workflows/release.yml";

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
        fs::copy(
            common::workspace_root().join(distribution::POLICY_PATH),
            root.join(distribution::POLICY_PATH),
        )?;
        Ok(Self {
            root,
            _directory: directory,
        })
    }
    pub fn package(&self, target: &str, destination: &str) -> TestResult<PathBuf> {
        self.package_bytes(target, destination, &executable_header(target))
    }
    pub fn package_bytes(
        &self,
        target: &str,
        destination: &str,
        bytes: &[u8],
    ) -> TestResult<PathBuf> {
        let bin = self.root.join(format!("test-binary-{target}"));
        let worker = self.root.join(format!("test-worker-{target}"));
        fs::write(&bin, bytes)?;
        fs::write(&worker, bytes)?;
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
    pub fn signature(&self, path: &Path) -> TestResult<distribution::SignatureReceipt> {
        let manifest = release::archive::verify_archive(path)?;
        let receipt = distribution::verify_signatures_with(
            path,
            &self.root,
            &manifest.target,
            &signing_tools(),
            &mut Callback(refuse_processes),
        )?;
        let parent = path.parent().ok_or("archive parent")?;
        save(
            &parent.join(format!("signature-{}.json", receipt.target)),
            &receipt,
        )?;
        Ok(receipt)
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

pub fn signing_tools() -> distribution::SigningTools {
    distribution::SigningTools {
        codesign: PathBuf::from("/usr/bin/codesign"),
        spctl: PathBuf::from("/usr/sbin/spctl"),
        powershell: Some(PathBuf::from(
            "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        )),
    }
}

pub fn refuse_processes(
    _: &[OsString],
    _: &Path,
    _: &ProcessLimits,
    _: Option<&BTreeMap<OsString, OsString>>,
) -> common::Result<ProcessResult> {
    Err(common::ToolError::new(
        "UNEXPECTED_PROCESS",
        "This operation must not start a process",
    ))
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

pub const SIGNATURE_AT: usize = 256;

pub fn put_le(bytes: &mut Vec<u8>, offset: usize, value: u32) {
    if bytes.len() < offset + 4 {
        bytes.resize(offset + 4, 0);
    }
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put_be(bytes: &mut Vec<u8>, offset: usize, value: u32) {
    if bytes.len() < offset + 4 {
        bytes.resize(offset + 4, 0);
    }
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

pub fn code_signature(flags: u32, cms: Option<usize>) -> Vec<u8> {
    let slots: u32 = if cms.is_some() { 2 } else { 1 };
    let directory = 12 + 8 * slots as usize;
    let directory_length = 88usize;
    let mut blob = Vec::new();
    put_be(&mut blob, 0, 0xfade_0cc0);
    put_be(&mut blob, 8, slots);
    put_be(&mut blob, 12, 0);
    put_be(&mut blob, 16, directory as u32);
    put_be(&mut blob, directory, 0xfade_0c02);
    put_be(&mut blob, directory + 4, directory_length as u32);
    put_be(&mut blob, directory + 8, 0x20400);
    put_be(&mut blob, directory + 12, flags);
    blob.resize(directory + directory_length, 0);
    if let Some(length) = cms {
        let wrapper = blob.len();
        put_be(&mut blob, 20, 0x1_0000);
        put_be(&mut blob, 24, wrapper as u32);
        put_be(&mut blob, wrapper, 0xfade_0b01);
        put_be(&mut blob, wrapper + 4, (8 + length) as u32);
        blob.resize(wrapper + 8 + length, 0x30);
    }
    let total = blob.len() as u32;
    put_be(&mut blob, 4, total);
    blob
}

pub fn signed_mach_o(target: &str, signature: &[u8]) -> Vec<u8> {
    let mut bytes = executable_header(target);
    put_le(&mut bytes, 16, 1);
    put_le(&mut bytes, 20, 16);
    put_le(&mut bytes, 32, 0x1d);
    put_le(&mut bytes, 36, 16);
    put_le(&mut bytes, 40, SIGNATURE_AT as u32);
    put_le(&mut bytes, 44, signature.len() as u32);
    bytes.resize(SIGNATURE_AT, 0);
    bytes.extend_from_slice(signature);
    bytes
}

pub fn signed_portable_executable(certificate: Option<(u32, u16, u16)>) -> Vec<u8> {
    let mut bytes = executable_header("x86_64-pc-windows-msvc");
    bytes[148..150].copy_from_slice(&240u16.to_le_bytes());
    put_le(&mut bytes, 260, 16);
    if let Some((length, revision, kind)) = certificate {
        put_le(&mut bytes, 296, 512);
        put_le(&mut bytes, 300, 64);
        bytes.resize(512, 0);
        put_le(&mut bytes, 512, length);
        bytes.extend_from_slice(&revision.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.resize(576, 0x30);
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

/// A deterministic stand-in for the engine that answers the commands quality measurement runs.
#[derive(Clone)]
pub struct FakeEngine {
    pub tables: u64,
    pub text: String,
    pub warnings: Vec<&'static str>,
    pub width_pt: f64,
    pub height_pt: f64,
    pub render_px: (u32, u32),
    pub self_diff_changes: u64,
    pub pair_diff_changes: u64,
    pub alternate_text: bool,
    pub failing_extension: Option<&'static str>,
    pub rejection: (i64, &'static str),
    pub calls: u64,
    pub text_calls: u64,
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self {
            tables: 2,
            text: "Quarterly figures".to_owned(),
            warnings: vec!["DOCX_FONT_SUBSTITUTED"],
            width_pt: 10.2,
            height_pt: 20.0,
            render_px: (11, 20),
            self_diff_changes: 0,
            pair_diff_changes: 3,
            alternate_text: false,
            failing_extension: None,
            rejection: (10, "UNSUPPORTED_FORMAT"),
            calls: 0,
            text_calls: 0,
        }
    }
}

impl FakeEngine {
    fn envelope(result: Value, warnings: &[&str]) -> common::Result<ProcessResult> {
        let warnings: Vec<Value> = warnings
            .iter()
            .map(|code| json!({"code": code, "severity": "warning", "message": "m", "effect": "e"}))
            .collect();
        Ok(outcome(
            0,
            json_bytes(&json!({
                "schema": "docsight.agent/v2",
                "result": result,
                "warnings": warnings,
                "limits": {"truncated": false},
            }))?,
            Vec::new(),
        ))
    }

    pub fn respond(&mut self, arguments: &[OsString]) -> common::Result<ProcessResult> {
        self.calls += 1;
        let contains = |name: &str| arguments.iter().any(|argument| argument == name);
        if contains("--version") {
            return Ok(version());
        }
        let documents: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .filter(|argument| {
                [".docx", ".pdf", ".bin"]
                    .iter()
                    .any(|extension| argument.ends_with(extension))
            })
            .collect();
        if documents
            .iter()
            .any(|document| !Path::new(document).is_absolute())
        {
            return Ok(outcome(
                40,
                Vec::new(),
                json_bytes(&json!({
                    "schema": "docsight.agent/v2",
                    "error": {"code": "IO_ERROR", "exit_code": 40},
                }))?,
            ));
        }
        let first = documents.first().cloned().unwrap_or_default();
        if first.ends_with(".bin") {
            return Ok(outcome(
                self.rejection.0,
                Vec::new(),
                json_bytes(&json!({
                    "schema": "docsight.agent/v2",
                    "error": {"code": self.rejection.1, "exit_code": self.rejection.0},
                }))?,
            ));
        }
        if self
            .failing_extension
            .is_some_and(|extension| first.ends_with(extension))
        {
            return Ok(outcome(
                11,
                Vec::new(),
                json_bytes(&json!({
                    "schema": "docsight.agent/v2",
                    "error": {"code": "MALFORMED_DOCUMENT", "exit_code": 11},
                }))?,
            ));
        }
        if contains("diff") {
            let changes = if documents.len() == 2 && documents[0] == documents[1] {
                self.self_diff_changes
            } else {
                self.pair_diff_changes
            };
            let counters = json!({"added": 0, "modified": changes, "moved": 0, "removed": 0});
            return Self::envelope(
                json!({"summary": {
                    "semantic_changes": changes,
                    "layout_changed_pages": 0,
                    "pages_before": 2,
                    "pages_after": 2,
                    "tables": counters,
                    "images": counters,
                }}),
                &[],
            );
        }
        if contains("render") {
            let index = arguments
                .iter()
                .position(|argument| argument == "--out")
                .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output"))?;
            let path = arguments
                .get(index + 1)
                .ok_or_else(|| common::ToolError::new("TEST_ARGUMENT", "Missing output path"))?;
            let (width, height) = self.render_px;
            let pixels = vec![200u8; (width * height * 3) as usize];
            let png = docsight_core::encode_png(width, height, &pixels)
                .map_err(|_| common::ToolError::new("TEST_PNG", "Cannot encode test PNG"))?;
            fs::write(path, png)?;
            return Self::envelope(
                json!({"page": 1, "dpi": 72, "width_px": width, "height_px": height}),
                &[],
            );
        }
        if contains("page") {
            return Self::envelope(
                json!({"number": 1, "width_pt": self.width_pt, "height_pt": self.height_pt, "spans": [{}, {}, {}]}),
                &[],
            );
        }
        if contains("text") {
            self.text_calls += 1;
            let content = if self.alternate_text && self.text_calls.is_multiple_of(2) {
                format!("{} (variant)", self.text)
            } else {
                self.text.clone()
            };
            return Self::envelope(
                json!({"blocks": [{"id": "p_1", "kind": "paragraph", "text": content}]}),
                &[],
            );
        }
        Self::envelope(
            json!({
                "format": if first.ends_with(".pdf") { "pdf" } else { "docx" },
                "pages": 2,
                "paragraphs": 5,
                "headings": 1,
                "tables": self.tables,
                "figures": 0,
                "blocks_by_kind": {"paragraph": 5, "table": self.tables},
            }),
            &self.warnings,
        )
    }
}

pub fn codesign_description(team: &str, timestamp: bool) -> Vec<u8> {
    let mut lines = vec![
        "Executable=/private/tmp/package/docsight".to_owned(),
        "Identifier=docsight".to_owned(),
        "Format=Mach-O thin (arm64)".to_owned(),
        "CodeDirectory v=20500 size=77646 flags=0x10000(runtime) hashes=2415+2 location=embedded"
            .to_owned(),
        "Signature size=9059".to_owned(),
        format!("Authority=Developer ID Application: Example Publisher ({team})"),
        "Authority=Developer ID Certification Authority".to_owned(),
        "Authority=Apple Root CA".to_owned(),
    ];
    if timestamp {
        lines.push("Timestamp=Sep 18, 2026 at 10:00:00".to_owned());
    } else {
        lines.push("Signed Time=Sep 18, 2026 at 10:00:00".to_owned());
    }
    lines.push(format!("TeamIdentifier={team}"));
    lines.push("Runtime Version=15.0.0".to_owned());
    (lines.join("\n") + "\n").into_bytes()
}

pub fn verification(archive_sha256: &str) -> Value {
    json!([{
        "attestation": {"bundle": {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json"}},
        "verificationResult": {
            "mediaType": "application/vnd.dev.sigstore.verificationresult+json;version=0.1",
            "signature": {"certificate": {
                "certificateIssuer": "CN=sigstore-intermediate,O=sigstore.dev",
                "subjectAlternativeName": format!("https://github.com/{SIGNER}@refs/heads/main"),
                "issuer": "https://token.actions.githubusercontent.com",
                "githubWorkflowRepository": REPOSITORY,
                "githubWorkflowRef": "refs/heads/main",
                "buildSignerURI": format!("https://github.com/{SIGNER}@refs/heads/main"),
                "runnerEnvironment": "github-hosted",
                "sourceRepositoryURI": format!("https://github.com/{REPOSITORY}"),
                "sourceRepositoryDigest": REVISION,
                "sourceRepositoryRef": "refs/heads/main",
                "runInvocationURI": format!("https://github.com/{REPOSITORY}/actions/runs/1/attempts/1"),
                "sourceRepositoryVisibilityAtSigning": "public"
            }},
            "verifiedTimestamps": [{"type": "Tlog", "uri": "https://rekor.sigstore.dev", "timestamp": "2026-09-18T10:00:00Z"}],
            "verifiedIdentity": {},
            "statement": {
                "_type": "https://in-toto.io/Statement/v1",
                "subject": [
                    {"name": "SHA256SUMS", "digest": {"sha256": "0".repeat(64)}},
                    {"name": "docsight.zip", "digest": {"sha256": archive_sha256}}
                ],
                "predicateType": xtask::release::provenance::SLSA_PROVENANCE,
                "predicate": {}
            }
        }
    }])
}

pub fn distributable_binary(target: &str) -> Vec<u8> {
    if target.ends_with("-apple-darwin") {
        signed_mach_o(target, &code_signature(0x1_0000, Some(512)))
    } else if target.ends_with("-windows-msvc") {
        signed_portable_executable(Some((64, 0x0200, 2)))
    } else {
        executable_header(target)
    }
}

pub fn require_authenticated_distribution(root: &Path) -> TestResult {
    let path = root.join(distribution::POLICY_PATH);
    let mut policy: distribution::Policy = serde_json::from_slice(&fs::read(&path)?)?;
    policy.provenance.status = distribution::ProvenanceStatus::Required;
    for entry in &mut policy.signing {
        if entry.mechanism == distribution::SigningMechanism::DeveloperIdNotarized {
            entry.status = distribution::SigningStatus::Required;
            entry.publisher = Some(TEAM.into());
        } else if entry.mechanism == distribution::SigningMechanism::Authenticode {
            entry.status = distribution::SigningStatus::Required;
            entry.publisher = Some(SUBJECT.into());
        }
    }
    fs::remove_file(&path)?;
    save(&path, &policy)
}

pub fn trusted_platform(
    arguments: &[OsString],
    _: &Path,
    _: &ProcessLimits,
    _: Option<&BTreeMap<OsString, OsString>>,
) -> common::Result<ProcessResult> {
    let program = arguments[0].to_string_lossy().into_owned();
    let encode = |value: &Value| {
        serde_json::to_vec(value).map_err(|_| common::ToolError::new("TEST_JSON", "cannot encode"))
    };
    if program == "/usr/bin/codesign" && arguments[1] == "--verify" {
        Ok(outcome(0, Vec::new(), Vec::new()))
    } else if program == "/usr/bin/codesign" {
        Ok(outcome(0, Vec::new(), codesign_description(TEAM, true)))
    } else if program == "/usr/sbin/spctl" {
        Ok(outcome(
            0,
            Vec::new(),
            b"docsight: accepted\nsource=Notarized Developer ID\n".to_vec(),
        ))
    } else if program.ends_with("powershell.exe") {
        Ok(outcome(
            0,
            encode(&json!({"status": "Valid", "subject": SUBJECT, "timestamped": true}))?,
            Vec::new(),
        ))
    } else if program.ends_with("gh") {
        let archive = sha256_file(Path::new(&arguments[3]))?;
        Ok(outcome(0, encode(&verification(&archive))?, Vec::new()))
    } else {
        Err(common::ToolError::new(
            "UNEXPECTED_PROCESS",
            "unexpected tool",
        ))
    }
}

impl Fixture {
    pub fn distribution(&self, path: &Path) -> TestResult {
        let manifest = release::archive::verify_archive(path)?;
        let signature = distribution::verify_signatures_with(
            path,
            &self.root,
            &manifest.target,
            &signing_tools(),
            &mut Callback(trusted_platform),
        )?;
        let provenance = release::provenance::verify_provenance_with(
            path,
            &self.root,
            Path::new("/usr/bin/gh"),
            &mut Callback(trusted_platform),
        )?;
        let parent = path.parent().ok_or("archive parent")?;
        save(
            &parent.join(format!("signature-{}.json", manifest.target)),
            &signature,
        )?;
        save(
            &parent.join(format!("provenance-{}.json", manifest.target)),
            &provenance,
        )
    }
}

pub fn relabel(archive: &Path, version: &str) -> TestResult<PathBuf> {
    let manifest = release::archive::verify_archive(archive)?;
    let old_base = release::basename(&manifest.version, &manifest.target)?;
    let new_base = release::basename(version, &manifest.target)?;
    let path = archive.with_file_name(format!("{new_base}.zip"));
    fs::copy(archive, &path)?;
    rewrite_archive(&path, |entries| {
        for entry in entries.iter_mut() {
            entry.name = entry.name.replacen(&old_base, &new_base, 1);
            if entry.name.ends_with("/release-manifest.json") {
                let mut value: Value = serde_json::from_slice(&entry.bytes).unwrap_or(Value::Null);
                value["version"] = json!(version);
                entry.bytes = json_bytes(&value).unwrap_or_default();
            }
        }
    })?;
    fs::write(
        path.with_file_name(format!("{new_base}.zip.sha256")),
        format!("{}  {new_base}.zip\n", sha256_file(&path)?),
    )?;
    release::archive::verify_archive(&path)?;
    Ok(path)
}

pub fn installed_version(binary: &Path) -> String {
    let directory = binary
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    directory
        .strip_prefix("docsight-")
        .and_then(|rest| rest.split_once('-'))
        .map(|(version, _)| version.to_owned())
        .unwrap_or_default()
}
