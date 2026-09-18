use super::archive::{Manifest, extract_verified, verify_archive};
use super::signature::{EmbeddedSignature, inspect};
use super::{TARGETS, binary_names};
use crate::tooling::common::*;
use crate::tooling::process::{ProcessLimits, ProcessResult, Runner};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const POLICY_PATH: &str = "release/distribution-policy.json";
pub const POLICY_SCHEMA: &str = "docsight.distribution-policy/v1";
pub const SIGNATURE_SCHEMA: &str = "docsight.release-signature/v1";
pub const SIGNED_FILE_VARIABLE: &str = "DOCSIGHT_SIGNED_FILE";
const AUTHENTICODE_QUERY: &str = "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false; $signature = Get-AuthenticodeSignature -LiteralPath $env:DOCSIGHT_SIGNED_FILE; [pscustomobject]@{status = [string]$signature.Status; subject = [string]$signature.SignerCertificate.Subject; timestamped = ($null -ne $signature.TimeStamperCertificate)} | ConvertTo-Json -Compress";
const NOTARIZED_SOURCE: &str = "source=Notarized Developer ID";
const DEVELOPER_AUTHORITY: &str = "Developer ID Application: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningMechanism {
    Authenticode,
    DeveloperIdNotarized,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningStatus {
    Required,
    PendingCredential,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceMechanism {
    GithubArtifactAttestation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceStatus {
    Required,
    PendingAttestationSupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub mechanism: ProvenanceMechanism,
    pub status: ProvenanceStatus,
    pub repository: String,
    pub signer_workflow: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSigning {
    pub target: String,
    pub mechanism: SigningMechanism,
    pub status: SigningStatus,
    #[serde(deserialize_with = "required_option")]
    pub publisher: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: String,
    pub provenance: Provenance,
    pub signing: Vec<TargetSigning>,
}

impl Policy {
    pub fn target(&self, target: &str) -> Result<&TargetSigning> {
        self.signing
            .iter()
            .find(|entry| entry.target == target)
            .ok_or_else(|| {
                ToolError::new(
                    "UNSUPPORTED_TARGET",
                    "Target is not part of the distribution policy",
                )
            })
    }
}

pub struct LoadedPolicy {
    pub policy: Policy,
    pub sha256: String,
}

fn invalid_policy(message: &'static str) -> ToolError {
    ToolError::new("INVALID_DISTRIBUTION_POLICY", message)
}

fn expected_mechanism(target: &str) -> SigningMechanism {
    if target.ends_with("-windows-msvc") {
        SigningMechanism::Authenticode
    } else if target.ends_with("-apple-darwin") {
        SigningMechanism::DeveloperIdNotarized
    } else {
        SigningMechanism::None
    }
}

fn valid_publisher(mechanism: SigningMechanism, publisher: &str) -> bool {
    match mechanism {
        SigningMechanism::DeveloperIdNotarized => {
            publisher.len() == 10
                && publisher
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        }
        SigningMechanism::Authenticode => {
            (4..=512).contains(&publisher.len())
                && publisher.starts_with("CN=")
                && publisher.chars().all(|character| !character.is_control())
                && publisher.trim() == publisher
        }
        SigningMechanism::None => false,
    }
}

fn valid_repository(repository: &str) -> bool {
    let mut parts = repository.split('/');
    let valid_part = |part: Option<&str>| {
        part.is_some_and(|part| {
            (1..=100).contains(&part.len())
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
    };
    valid_part(parts.next()) && valid_part(parts.next()) && parts.next().is_none()
}

pub fn validate_policy(policy: &Policy) -> Result<()> {
    require(
        policy.schema == POLICY_SCHEMA,
        "INVALID_DISTRIBUTION_POLICY",
        "Distribution policy schema is unsupported",
    )?;
    require(
        valid_repository(&policy.provenance.repository)
            && policy
                .provenance
                .signer_workflow
                .starts_with(".github/workflows/")
            && policy.provenance.signer_workflow.ends_with(".yml")
            && safe_member(&policy.provenance.signer_workflow).is_ok(),
        "INVALID_DISTRIBUTION_POLICY",
        "Provenance must name one repository and one release workflow",
    )?;
    require(
        policy.signing.len() == TARGETS.len()
            && policy
                .signing
                .iter()
                .zip(TARGETS)
                .all(|(entry, target)| entry.target == target),
        "INVALID_DISTRIBUTION_POLICY",
        "Signing policy must list the five release targets in canonical order",
    )?;
    for entry in &policy.signing {
        if entry.mechanism != expected_mechanism(&entry.target) {
            return Err(invalid_policy(
                "Signing mechanism does not match the target platform",
            ));
        }
        let consistent = match (entry.mechanism, entry.status, entry.publisher.as_deref()) {
            (SigningMechanism::None, SigningStatus::NotApplicable, None) => true,
            (SigningMechanism::None, _, _) | (_, SigningStatus::NotApplicable, _) => false,
            (_, SigningStatus::PendingCredential, publisher) => publisher.is_none(),
            (mechanism, SigningStatus::Required, publisher) => {
                publisher.is_some_and(|publisher| valid_publisher(mechanism, publisher))
            }
        };
        if !consistent {
            return Err(invalid_policy(
                "Required signing names a valid publisher; pending and unsigned targets name none",
            ));
        }
    }
    Ok(())
}

pub fn load_policy(root: &Path) -> Result<LoadedPolicy> {
    let path = root.join(POLICY_PATH);
    let policy: Policy = decode(read_json(&path)?)?;
    validate_policy(&policy)?;
    Ok(LoadedPolicy {
        policy,
        sha256: sha256_file(&path)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignatureStatus {
    Verified,
    PendingCredential,
    NotApplicable,
    Failed,
}

impl SignatureStatus {
    pub fn expected(status: SigningStatus) -> Self {
        match status {
            SigningStatus::Required => Self::Verified,
            SigningStatus::PendingCredential => Self::PendingCredential,
            SigningStatus::NotApplicable => Self::NotApplicable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableSignature {
    pub path: String,
    pub sha256: String,
    pub embedded: EmbeddedSignature,
    #[serde(deserialize_with = "required_option")]
    pub hardened_runtime: Option<bool>,
    #[serde(deserialize_with = "required_option")]
    pub publisher: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub timestamped: Option<bool>,
    #[serde(deserialize_with = "required_option")]
    pub notarized: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureReceipt {
    pub schema: String,
    pub version: String,
    pub target: String,
    pub revision: String,
    pub archive_sha256: String,
    pub policy_sha256: String,
    pub mechanism: SigningMechanism,
    pub status: SignatureStatus,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<String>,
    pub executables: Vec<ExecutableSignature>,
}

pub struct SigningTools {
    pub codesign: PathBuf,
    pub spctl: PathBuf,
    pub powershell: Option<PathBuf>,
}

impl SigningTools {
    pub fn native() -> Self {
        Self {
            codesign: PathBuf::from("/usr/bin/codesign"),
            spctl: PathBuf::from("/usr/sbin/spctl"),
            powershell: std::env::var_os("SYSTEMROOT").map(|root| {
                PathBuf::from(root)
                    .join("System32")
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe")
            }),
        }
    }
}

struct Verification<'a, R: Runner> {
    tools: &'a SigningTools,
    work: &'a Path,
    runner: &'a mut R,
}

impl<R: Runner> Verification<'_, R> {
    fn run(
        &mut self,
        arguments: Vec<OsString>,
        environment: Option<&BTreeMap<OsString, OsString>>,
        seconds: u64,
    ) -> Result<ProcessResult> {
        let result = self.runner.run(
            &arguments,
            self.work,
            &ProcessLimits {
                timeout: Duration::from_secs(seconds),
                output_bytes: 1_048_576,
            },
            environment,
        )?;
        require(
            result.termination.is_none(),
            "SIGNATURE_VERIFIER_LIMIT",
            "Signature verifier exceeded its execution budget",
        )?;
        Ok(result)
    }

    fn developer_id(
        &mut self,
        path: &Path,
        team: &str,
        record: &mut ExecutableSignature,
    ) -> Result<()> {
        let verify = self.run(
            vec![
                self.tools.codesign.clone().into_os_string(),
                "--verify".into(),
                "--strict".into(),
                "--verbose=2".into(),
                path.as_os_str().to_owned(),
            ],
            None,
            60,
        )?;
        require(
            verify.returncode == 0,
            "SIGNATURE_INVALID",
            "codesign rejected the executable signature",
        )?;
        let display = self.run(
            vec![
                self.tools.codesign.clone().into_os_string(),
                "--display".into(),
                "--verbose=4".into(),
                path.as_os_str().to_owned(),
            ],
            None,
            60,
        )?;
        require(
            display.returncode == 0,
            "SIGNATURE_INVALID",
            "codesign could not describe the executable signature",
        )?;
        let fields = key_values(&display)?;
        let team_identifier = fields
            .get("TeamIdentifier")
            .and_then(|values| values.first());
        let authority = fields.get("Authority").and_then(|values| values.first());
        record.publisher = team_identifier.map(|value| (*value).to_owned());
        record.timestamped = Some(fields.contains_key("Timestamp"));
        require(
            team_identifier.map(String::as_str) == Some(team)
                && authority.is_some_and(|authority| {
                    authority.starts_with(DEVELOPER_AUTHORITY)
                        && authority.ends_with(&format!("({team})"))
                }),
            "PUBLISHER_MISMATCH",
            "Signature does not name the Developer ID team in the distribution policy",
        )?;
        require(
            record.timestamped == Some(true),
            "SIGNATURE_NOT_TIMESTAMPED",
            "Signature lacks a secure timestamp",
        )?;
        require(
            record.hardened_runtime == Some(true),
            "HARDENED_RUNTIME_MISSING",
            "Notarized executables must enable the hardened runtime",
        )?;
        let assessment = self.run(
            vec![
                self.tools.spctl.clone().into_os_string(),
                "--assess".into(),
                "--type".into(),
                "install".into(),
                "--verbose=2".into(),
                path.as_os_str().to_owned(),
            ],
            None,
            180,
        )?;
        let notarized = assessment.returncode == 0
            && combined_lines(&assessment)?
                .iter()
                .any(|line| line.trim() == NOTARIZED_SOURCE);
        record.notarized = Some(notarized);
        require(
            notarized,
            "NOTARIZATION_MISSING",
            "Gatekeeper does not accept the executable as notarized Developer ID code",
        )
    }

    fn authenticode(
        &mut self,
        path: &Path,
        subject: &str,
        record: &mut ExecutableSignature,
    ) -> Result<()> {
        let powershell = self.tools.powershell.clone().ok_or_else(|| {
            ToolError::new(
                "MISSING_SYSTEMROOT",
                "Windows signature verification requires SYSTEMROOT",
            )
        })?;
        let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        environment.insert(SIGNED_FILE_VARIABLE.into(), path.as_os_str().to_owned());
        let query = self.run(
            vec![
                powershell.into_os_string(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                AUTHENTICODE_QUERY.into(),
            ],
            Some(&environment),
            120,
        )?;
        require(
            query.returncode == 0,
            "SIGNATURE_VERIFIER_FAILED",
            "PowerShell could not query the Authenticode signature",
        )?;
        let answer = parse_json(&query.stdout)?;
        exact_keys(&answer, &["status", "subject", "timestamped"])?;
        let status = string(field(&answer, "status")?)?;
        let signer = string(field(&answer, "subject")?)?;
        record.publisher = Some(signer.to_owned()).filter(|signer| !signer.is_empty());
        record.timestamped = field(&answer, "timestamped")?.as_bool();
        require(
            status == "Valid",
            "SIGNATURE_INVALID",
            "Windows does not report the Authenticode signature as valid",
        )?;
        require(
            signer == subject,
            "PUBLISHER_MISMATCH",
            "Signature does not name the certificate subject in the distribution policy",
        )?;
        require(
            record.timestamped == Some(true),
            "SIGNATURE_NOT_TIMESTAMPED",
            "Signature lacks a secure timestamp",
        )
    }
}

fn combined_lines(result: &ProcessResult) -> Result<Vec<String>> {
    let mut lines: Vec<String> = text(&result.stdout)?.lines().map(str::to_owned).collect();
    lines.extend(text(&result.stderr)?.lines().map(str::to_owned));
    Ok(lines)
}

fn key_values(result: &ProcessResult) -> Result<BTreeMap<String, Vec<String>>> {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in combined_lines(result)? {
        if let Some((key, value)) = line.split_once('=') {
            fields
                .entry(key.trim().to_owned())
                .or_default()
                .push(value.trim().to_owned());
        }
    }
    Ok(fields)
}

fn examine<R: Runner>(
    entry: &TargetSigning,
    record: &mut ExecutableSignature,
    path: &Path,
    verification: &mut Verification<'_, R>,
) -> Result<()> {
    match entry.status {
        SigningStatus::NotApplicable => require(
            record.embedded == EmbeddedSignature::NotApplicable,
            "UNEXPECTED_SIGNATURE",
            "Executable format carries no platform signature on this target",
        ),
        SigningStatus::PendingCredential => require(
            record.embedded != EmbeddedSignature::Cms,
            "UNEXPECTED_SIGNATURE",
            "Executable carries a signature the distribution policy does not declare",
        ),
        SigningStatus::Required => {
            require(
                record.embedded == EmbeddedSignature::Cms,
                "SIGNATURE_MISSING",
                "Executable lacks the signature the distribution policy requires",
            )?;
            let publisher = entry
                .publisher
                .as_deref()
                .ok_or_else(|| invalid_policy("Required signing must name its publisher"))?;
            match entry.mechanism {
                SigningMechanism::DeveloperIdNotarized => {
                    verification.developer_id(path, publisher, record)
                }
                SigningMechanism::Authenticode => {
                    verification.authenticode(path, publisher, record)
                }
                SigningMechanism::None => Err(invalid_policy(
                    "A target without platform signing cannot require a signature",
                )),
            }
        }
    }
}

pub fn verify_signatures_with<R: Runner>(
    archive: &Path,
    root: &Path,
    host: &str,
    tools: &SigningTools,
    runner: &mut R,
) -> Result<SignatureReceipt> {
    let loaded = load_policy(root)?;
    let manifest = verify_archive(archive)?;
    require(
        manifest.target == host,
        "SIGNATURE_HOST_MISMATCH",
        "Signatures are verified on the native host of their target",
    )?;
    let entry = loaded.policy.target(&manifest.target)?;
    let archive_sha256 = sha256_file(archive)?;
    let temporary = tempfile::tempdir()?;
    let base = temporary.path().canonicalize()?;
    let package = base.join("package");
    extract_verified(archive, &package)?;
    let work = base.join("work");
    std::fs::create_dir(&work)?;
    let mut verification = Verification {
        tools,
        work: &work,
        runner,
    };
    let (binary, worker) = binary_names(&manifest.target)?;
    let mut executables = Vec::new();
    let mut failure = None;
    for name in [binary, worker] {
        let path = package.join(name);
        let bytes = read_bytes(&path, MAX_FILE_BYTES)?;
        let inspection = inspect(&bytes, &manifest.target)?;
        let mut record = ExecutableSignature {
            path: name.into(),
            sha256: digest(&bytes),
            embedded: inspection.embedded,
            hardened_runtime: inspection.hardened_runtime,
            publisher: None,
            timestamped: None,
            notarized: None,
        };
        if let Err(error) = examine(entry, &mut record, &path, &mut verification) {
            failure.get_or_insert(error.code);
        }
        executables.push(record);
    }
    require(
        sha256_file(archive)? == archive_sha256,
        "ARCHIVE_CHANGED",
        "Archive changed during signature verification",
    )?;
    let status = match failure {
        Some(_) => SignatureStatus::Failed,
        None => SignatureStatus::expected(entry.status),
    };
    Ok(SignatureReceipt {
        schema: SIGNATURE_SCHEMA.into(),
        version: manifest.version,
        target: manifest.target,
        revision: manifest.revision,
        archive_sha256,
        policy_sha256: loaded.sha256,
        mechanism: entry.mechanism,
        status,
        error_code: failure.map(str::to_owned),
        executables,
    })
}

pub fn verify_signatures<R: Runner>(
    archive: &Path,
    root: &Path,
    runner: &mut R,
) -> Result<SignatureReceipt> {
    verify_signatures_with(
        archive,
        root,
        super::native_target()?,
        &SigningTools::native(),
        runner,
    )
}

pub fn validate_signature_receipt(
    receipt: &SignatureReceipt,
    manifest: &Manifest,
    archive_sha256: &str,
    loaded: &LoadedPolicy,
) -> Result<()> {
    let entry = loaded.policy.target(&manifest.target)?;
    require(
        receipt.schema == SIGNATURE_SCHEMA
            && receipt.version == manifest.version
            && receipt.target == manifest.target
            && receipt.revision == manifest.revision
            && receipt.archive_sha256 == archive_sha256
            && receipt.policy_sha256 == loaded.sha256
            && receipt.mechanism == entry.mechanism,
        "INVALID_SIGNATURE_RECEIPT",
        "Signature receipt does not attest this exact candidate archive and policy",
    )?;
    let (binary, worker) = binary_names(&manifest.target)?;
    let declared: BTreeMap<&str, &str> = manifest
        .files
        .iter()
        .map(|record| (record.path.as_str(), record.sha256.as_str()))
        .collect();
    require(
        receipt.executables.len() == 2
            && receipt
                .executables
                .iter()
                .zip([binary, worker])
                .all(|(record, name)| {
                    record.path == name && declared.get(name) == Some(&record.sha256.as_str())
                }),
        "INVALID_SIGNATURE_RECEIPT",
        "Signature receipt must describe both packaged executables",
    )?;
    require(
        receipt.status == SignatureStatus::expected(entry.status) && receipt.error_code.is_none(),
        "SIGNATURE_POLICY_UNMET",
        "Signature verification does not satisfy the distribution policy",
    )
}
