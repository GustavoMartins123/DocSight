#[allow(dead_code)]
mod support;

use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use support::*;
use xtask::release::TARGETS;
use xtask::release::archive::collect;
use xtask::release::distribution::{
    POLICY_PATH, Policy, ProvenanceStatus, SIGNED_FILE_VARIABLE, SignatureReceipt, SignatureStatus,
    SigningMechanism, SigningStatus, load_policy, validate_policy, verify_signatures_with,
};
use xtask::release::signature::EmbeddedSignature;
use xtask::tooling::common::{ToolError, workspace_root};
use xtask::tooling::process::{ProcessLimits, ProcessResult, Runner};

type PolicyEdit = Box<dyn Fn(&mut Policy)>;
type ReceiptEdit = Box<dyn Fn(&mut SignatureReceipt)>;

const MAC: &str = "aarch64-apple-darwin";
const WINDOWS: &str = "x86_64-pc-windows-msvc";
const LINUX: &str = "x86_64-unknown-linux-gnu";
const TEAM: &str = "ABCDE12345";
const SUBJECT: &str = "CN=Example Publisher, O=Example Publisher, L=Sao Paulo, C=BR";
const RUNTIME: u32 = 0x1_0000;
const ADHOC: u32 = 0x2;

fn repository_policy() -> TestResult<Policy> {
    Ok(serde_json::from_slice(&fs::read(
        workspace_root().join(POLICY_PATH),
    )?)?)
}

fn require_signing(fixture: &Fixture, target: &str, publisher: &str) -> TestResult {
    let mut policy = repository_policy()?;
    let entry = policy
        .signing
        .iter_mut()
        .find(|entry| entry.target == target)
        .ok_or("target")?;
    entry.status = SigningStatus::Required;
    entry.publisher = Some(publisher.into());
    let path = fixture.root.join(POLICY_PATH);
    fs::remove_file(&path)?;
    save(&path, &policy)
}

fn developer_binary(flags: u32) -> Vec<u8> {
    signed_mach_o(MAC, &code_signature(flags, Some(512)))
}

fn authenticode_binary() -> Vec<u8> {
    signed_portable_executable(Some((64, 0x0200, 2)))
}

fn codesign_description(team: &str, timestamp: bool) -> Vec<u8> {
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

#[derive(Clone)]
struct Apple {
    verify_code: i64,
    team: &'static str,
    timestamp: bool,
    assessment_code: i64,
    source: &'static str,
    timeout: bool,
}

impl Default for Apple {
    fn default() -> Self {
        Self {
            verify_code: 0,
            team: TEAM,
            timestamp: true,
            assessment_code: 0,
            source: "Notarized Developer ID",
            timeout: false,
        }
    }
}

struct AppleTools<'a> {
    apple: Apple,
    calls: &'a mut Vec<Vec<OsString>>,
}

impl Runner for AppleTools<'_> {
    fn run(
        &mut self,
        arguments: &[OsString],
        _: &Path,
        _: &ProcessLimits,
        _: Option<&BTreeMap<OsString, OsString>>,
    ) -> xtask::tooling::common::Result<ProcessResult> {
        self.calls.push(arguments.to_vec());
        let apple = &self.apple;
        let program = arguments[0].to_string_lossy().into_owned();
        let mut result = if program == "/usr/bin/codesign" && arguments[1] == "--verify" {
            outcome(apple.verify_code, Vec::new(), b"valid on disk\n".to_vec())
        } else if program == "/usr/bin/codesign" && arguments[1] == "--display" {
            outcome(
                0,
                Vec::new(),
                codesign_description(apple.team, apple.timestamp),
            )
        } else if program == "/usr/sbin/spctl" {
            outcome(
                apple.assessment_code,
                Vec::new(),
                format!(
                    "docsight: accepted\nsource={}\norigin=Developer ID Application: Example Publisher ({})\n",
                    apple.source, apple.team
                )
                .into_bytes(),
            )
        } else {
            return Err(ToolError::new("UNEXPECTED_PROCESS", "unexpected tool"));
        };
        if apple.timeout {
            result.termination = Some(xtask::tooling::process::Termination::Timeout);
        }
        Ok(result)
    }
}

fn verify_mac(fixture: &Fixture, archive: &Path, apple: Apple) -> TestResult<SignatureReceipt> {
    let mut calls = Vec::new();
    let receipt = verify_signatures_with(
        archive,
        &fixture.root,
        MAC,
        &signing_tools(),
        &mut AppleTools {
            apple,
            calls: &mut calls,
        },
    )?;
    Ok(receipt)
}

fn authenticode_answer(status: &str, subject: &str, timestamped: bool) -> Value {
    serde_json::json!({"status": status, "subject": subject, "timestamped": timestamped})
}

fn verify_windows(
    fixture: &Fixture,
    archive: &Path,
    answer: Value,
    signed_files: &mut Vec<PathBuf>,
) -> TestResult<SignatureReceipt> {
    let mut runner = Callback(
        |arguments: &[OsString],
         _: &Path,
         _: &ProcessLimits,
         environment: Option<&BTreeMap<OsString, OsString>>| {
            let program = arguments[0].to_string_lossy().into_owned();
            if !program.ends_with("WindowsPowerShell/v1.0/powershell.exe")
                || !arguments
                    .iter()
                    .any(|argument| argument == "-NonInteractive")
            {
                return Err(ToolError::new("UNEXPECTED_PROCESS", "unexpected tool"));
            }
            let file = environment
                .and_then(|environment| environment.get(&OsString::from(SIGNED_FILE_VARIABLE)))
                .ok_or_else(|| ToolError::new("UNEXPECTED_PROCESS", "missing signed file"))?;
            signed_files.push(PathBuf::from(file));
            Ok(outcome(
                0,
                serde_json::to_vec(&answer)
                    .map_err(|_| ToolError::new("TEST_JSON", "cannot encode"))?,
                Vec::new(),
            ))
        },
    );
    Ok(verify_signatures_with(
        archive,
        &fixture.root,
        WINDOWS,
        &signing_tools(),
        &mut runner,
    )?)
}

#[test]
fn repository_policy_keeps_unavailable_credentials_pending() -> TestResult {
    let loaded = load_policy(&workspace_root())?;
    assert_eq!(
        loaded.policy.provenance.status,
        ProvenanceStatus::PendingAttestationSupport
    );
    for (entry, target) in loaded.policy.signing.iter().zip(TARGETS) {
        assert_eq!(entry.target, target);
        assert_eq!(entry.publisher, None);
        let expected = if target.ends_with("-linux-gnu") {
            SigningStatus::NotApplicable
        } else {
            SigningStatus::PendingCredential
        };
        assert_eq!(entry.status, expected, "{target}");
    }
    Ok(())
}

#[test]
fn policy_validation_rejects_inconsistent_signing_entries() -> TestResult {
    let base = repository_policy()?;
    let edits: Vec<(&str, PolicyEdit)> = vec![
        (
            "platform mechanism",
            Box::new(|policy| policy.signing[0].mechanism = SigningMechanism::DeveloperIdNotarized),
        ),
        (
            "required without publisher",
            Box::new(|policy| policy.signing[4].status = SigningStatus::Required),
        ),
        (
            "pending with publisher",
            Box::new(|policy| policy.signing[4].publisher = Some(TEAM.into())),
        ),
        (
            "lowercase team",
            Box::new(|policy| {
                policy.signing[4].status = SigningStatus::Required;
                policy.signing[4].publisher = Some("abcde12345".into());
            }),
        ),
        (
            "short team",
            Box::new(|policy| {
                policy.signing[4].status = SigningStatus::Required;
                policy.signing[4].publisher = Some("ABCDE1234".into());
            }),
        ),
        (
            "subject without common name",
            Box::new(|policy| {
                policy.signing[0].status = SigningStatus::Required;
                policy.signing[0].publisher = Some("O=Example Publisher".into());
            }),
        ),
        (
            "unsigned platform required",
            Box::new(|policy| {
                policy.signing[1].status = SigningStatus::Required;
                policy.signing[1].publisher = Some(TEAM.into());
            }),
        ),
        (
            "signed platform not applicable",
            Box::new(|policy| policy.signing[3].status = SigningStatus::NotApplicable),
        ),
        ("target order", Box::new(|policy| policy.signing.swap(0, 1))),
        (
            "missing target",
            Box::new(|policy| {
                policy.signing.pop();
            }),
        ),
        (
            "repository",
            Box::new(|policy| policy.provenance.repository = "owner/name/extra".into()),
        ),
        (
            "workflow",
            Box::new(|policy| {
                policy.provenance.signer_workflow = ".github/workflows/../release.yml".into();
            }),
        ),
        (
            "schema",
            Box::new(|policy| policy.schema = "docsight.distribution-policy/v2".into()),
        ),
    ];
    for (name, edit) in edits {
        let mut policy = base.clone();
        edit(&mut policy);
        assert_eq!(
            code(validate_policy(&policy)),
            Some("INVALID_DISTRIBUTION_POLICY"),
            "{name}"
        );
    }
    let mut accepted = base;
    accepted.signing[0].status = SigningStatus::Required;
    accepted.signing[0].publisher = Some(SUBJECT.into());
    accepted.signing[4].status = SigningStatus::Required;
    accepted.signing[4].publisher = Some(TEAM.into());
    validate_policy(&accepted)?;
    Ok(())
}

#[test]
fn policy_files_reject_undeclared_fields() -> TestResult {
    let fixture = Fixture::new()?;
    let path = fixture.root.join(POLICY_PATH);
    let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    value["signing"][0]["thumbprint"] = Value::String("00".into());
    fs::write(&path, serde_json::to_vec(&value)?)?;
    assert!(load_policy(&fixture.root).is_err());
    Ok(())
}

#[test]
fn pending_and_unsigned_targets_are_recorded_without_platform_tools() -> TestResult {
    let fixture = Fixture::new()?;
    for (target, embedded, status) in [
        (
            MAC,
            EmbeddedSignature::Absent,
            SignatureStatus::PendingCredential,
        ),
        (
            WINDOWS,
            EmbeddedSignature::Absent,
            SignatureStatus::PendingCredential,
        ),
        (
            LINUX,
            EmbeddedSignature::NotApplicable,
            SignatureStatus::NotApplicable,
        ),
    ] {
        let archive = fixture.package(target, target)?;
        let receipt = verify_signatures_with(
            &archive,
            &fixture.root,
            target,
            &signing_tools(),
            &mut Callback(refuse_processes),
        )?;
        assert_eq!(receipt.status, status, "{target}");
        assert_eq!(receipt.error_code, None);
        assert_eq!(receipt.executables.len(), 2);
        assert!(
            receipt
                .executables
                .iter()
                .all(|record| record.embedded == embedded
                    && record.publisher.is_none()
                    && record.notarized.is_none())
        );
    }
    let linked = fixture.package_bytes(
        MAC,
        "linked",
        &signed_mach_o(MAC, &code_signature(ADHOC | 0x2_0000, None)),
    )?;
    let receipt = verify_signatures_with(
        &linked,
        &fixture.root,
        MAC,
        &signing_tools(),
        &mut Callback(refuse_processes),
    )?;
    assert_eq!(receipt.status, SignatureStatus::PendingCredential);
    assert_eq!(receipt.executables[0].embedded, EmbeddedSignature::AdHoc);
    Ok(())
}

#[test]
fn a_signature_the_policy_does_not_declare_fails_verification() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package_bytes(MAC, "dist", &developer_binary(RUNTIME))?;
    let receipt = verify_signatures_with(
        &archive,
        &fixture.root,
        MAC,
        &signing_tools(),
        &mut Callback(refuse_processes),
    )?;
    assert_eq!(receipt.status, SignatureStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("UNEXPECTED_SIGNATURE"));
    Ok(())
}

#[test]
fn notarized_developer_id_signatures_are_verified_against_the_policy_team() -> TestResult {
    let fixture = Fixture::new()?;
    require_signing(&fixture, MAC, TEAM)?;
    let archive = fixture.package_bytes(MAC, "dist", &developer_binary(RUNTIME))?;
    let mut calls = Vec::new();
    let receipt = verify_signatures_with(
        &archive,
        &fixture.root,
        MAC,
        &signing_tools(),
        &mut AppleTools {
            apple: Apple::default(),
            calls: &mut calls,
        },
    )?;
    assert_eq!(receipt.status, SignatureStatus::Verified, "{receipt:?}");
    assert_eq!(receipt.error_code, None);
    for record in &receipt.executables {
        assert_eq!(record.embedded, EmbeddedSignature::Cms);
        assert_eq!(record.hardened_runtime, Some(true));
        assert_eq!(record.publisher.as_deref(), Some(TEAM));
        assert_eq!(record.timestamped, Some(true));
        assert_eq!(record.notarized, Some(true));
    }
    assert_eq!(calls.len(), 6);
    for call in &calls {
        let path = PathBuf::from(call.last().ok_or("path")?);
        assert!(path.is_absolute());
        assert!(path.ends_with("package/docsight") || path.ends_with("package/docsight-worker"));
    }
    Ok(())
}

#[test]
fn developer_id_verification_fails_closed_on_each_missing_guarantee() -> TestResult {
    let fixture = Fixture::new()?;
    require_signing(&fixture, MAC, TEAM)?;
    let signed = fixture.package_bytes(MAC, "signed", &developer_binary(RUNTIME))?;
    let cases = [
        (
            Apple {
                verify_code: 1,
                ..Apple::default()
            },
            "SIGNATURE_INVALID",
        ),
        (
            Apple {
                team: "ZZZZZ99999",
                ..Apple::default()
            },
            "PUBLISHER_MISMATCH",
        ),
        (
            Apple {
                timestamp: false,
                ..Apple::default()
            },
            "SIGNATURE_NOT_TIMESTAMPED",
        ),
        (
            Apple {
                source: "Developer ID",
                ..Apple::default()
            },
            "NOTARIZATION_MISSING",
        ),
        (
            Apple {
                assessment_code: 3,
                ..Apple::default()
            },
            "NOTARIZATION_MISSING",
        ),
        (
            Apple {
                timeout: true,
                ..Apple::default()
            },
            "SIGNATURE_VERIFIER_LIMIT",
        ),
    ];
    for (apple, expected) in cases {
        let receipt = verify_mac(&fixture, &signed, apple)?;
        assert_eq!(receipt.status, SignatureStatus::Failed, "{expected}");
        assert_eq!(receipt.error_code.as_deref(), Some(expected));
    }
    let without_runtime = fixture.package_bytes(MAC, "runtime", &developer_binary(0))?;
    let receipt = verify_mac(&fixture, &without_runtime, Apple::default())?;
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("HARDENED_RUNTIME_MISSING")
    );
    let unsigned = fixture.package(MAC, "unsigned")?;
    let receipt = verify_mac(&fixture, &unsigned, Apple::default())?;
    assert_eq!(receipt.error_code.as_deref(), Some("SIGNATURE_MISSING"));
    assert!(
        receipt
            .executables
            .iter()
            .all(|record| record.notarized.is_none())
    );
    Ok(())
}

#[test]
fn authenticode_signatures_are_verified_against_the_policy_subject() -> TestResult {
    let fixture = Fixture::new()?;
    require_signing(&fixture, WINDOWS, SUBJECT)?;
    let archive = fixture.package_bytes(WINDOWS, "dist", &authenticode_binary())?;
    let mut signed_files = Vec::new();
    let receipt = verify_windows(
        &fixture,
        &archive,
        authenticode_answer("Valid", SUBJECT, true),
        &mut signed_files,
    )?;
    assert_eq!(receipt.status, SignatureStatus::Verified, "{receipt:?}");
    assert!(receipt.executables.iter().all(|record| {
        record.publisher.as_deref() == Some(SUBJECT)
            && record.timestamped == Some(true)
            && record.notarized.is_none()
    }));
    assert_eq!(signed_files.len(), 2);
    assert!(signed_files[0].ends_with("package/docsight.exe"));
    assert!(signed_files[1].ends_with("package/docsight-worker.exe"));

    for (answer, expected) in [
        (
            authenticode_answer("NotSigned", "", false),
            "SIGNATURE_INVALID",
        ),
        (
            authenticode_answer("HashMismatch", SUBJECT, true),
            "SIGNATURE_INVALID",
        ),
        (
            authenticode_answer("Valid", "CN=Someone Else", true),
            "PUBLISHER_MISMATCH",
        ),
        (
            authenticode_answer("Valid", SUBJECT, false),
            "SIGNATURE_NOT_TIMESTAMPED",
        ),
    ] {
        let receipt = verify_windows(&fixture, &archive, answer, &mut Vec::new())?;
        assert_eq!(receipt.status, SignatureStatus::Failed, "{expected}");
        assert_eq!(receipt.error_code.as_deref(), Some(expected));
    }
    let mut extra = authenticode_answer("Valid", SUBJECT, true);
    extra["thumbprint"] = Value::String("00".into());
    let receipt = verify_windows(&fixture, &archive, extra, &mut Vec::new())?;
    assert_eq!(receipt.status, SignatureStatus::Failed);
    Ok(())
}

#[test]
fn authenticode_verification_requires_the_system_powershell() -> TestResult {
    let fixture = Fixture::new()?;
    require_signing(&fixture, WINDOWS, SUBJECT)?;
    let archive = fixture.package_bytes(WINDOWS, "dist", &authenticode_binary())?;
    let mut tools = signing_tools();
    tools.powershell = None;
    let receipt = verify_signatures_with(
        &archive,
        &fixture.root,
        WINDOWS,
        &tools,
        &mut Callback(refuse_processes),
    )?;
    assert_eq!(receipt.status, SignatureStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("MISSING_SYSTEMROOT"));
    Ok(())
}

#[test]
fn signatures_are_verified_only_on_their_native_host() -> TestResult {
    let fixture = Fixture::new()?;
    let archive = fixture.package(MAC, "dist")?;
    assert_eq!(
        code(verify_signatures_with(
            &archive,
            &fixture.root,
            LINUX,
            &signing_tools(),
            &mut Callback(refuse_processes),
        )),
        Some("SIGNATURE_HOST_MISMATCH")
    );
    Ok(())
}

fn release_set(fixture: &Fixture, directory: &str) -> TestResult<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for target in TARGETS {
        let archive = fixture.package(target, directory)?;
        fixture.receipt(&archive)?;
        fixture.signature(&archive)?;
        archives.push(archive);
    }
    Ok(archives)
}

fn rewrite_signature(archive: &Path, edit: impl FnOnce(&mut SignatureReceipt)) -> TestResult {
    let manifest = xtask::release::archive::verify_archive(archive)?;
    let path = archive
        .parent()
        .ok_or("parent")?
        .join(format!("signature-{}.json", manifest.target));
    let mut receipt: SignatureReceipt = serde_json::from_slice(&fs::read(&path)?)?;
    edit(&mut receipt);
    fs::remove_file(&path)?;
    save(&path, &receipt)
}

#[test]
fn release_assembly_requires_signature_receipts_that_satisfy_the_policy() -> TestResult {
    let fixture = Fixture::new()?;
    release_set(&fixture, "dist")?;
    let summary = collect(&fixture.root.join("dist"), &fixture.root)?;
    assert_eq!(summary["provenance"], "pending-attestation-support");
    assert_eq!(summary["signatures"][MAC], "pending-credential");
    assert_eq!(summary["signatures"][LINUX], "not-applicable");

    let cases: Vec<(&str, ReceiptEdit)> = vec![
        (
            "INVALID_SIGNATURE_RECEIPT",
            Box::new(|receipt| receipt.archive_sha256 = "f".repeat(64)),
        ),
        (
            "INVALID_SIGNATURE_RECEIPT",
            Box::new(|receipt| receipt.policy_sha256 = "e".repeat(64)),
        ),
        (
            "INVALID_SIGNATURE_RECEIPT",
            Box::new(|receipt| receipt.executables[0].sha256 = "d".repeat(64)),
        ),
        (
            "INVALID_SIGNATURE_RECEIPT",
            Box::new(|receipt| {
                receipt.executables.pop();
            }),
        ),
        (
            "SIGNATURE_POLICY_UNMET",
            Box::new(|receipt| {
                receipt.status = SignatureStatus::Failed;
                receipt.error_code = Some("SIGNATURE_MISSING".into());
            }),
        ),
        (
            "SIGNATURE_POLICY_UNMET",
            Box::new(|receipt| receipt.status = SignatureStatus::Verified),
        ),
    ];
    for (index, (expected, edit)) in cases.into_iter().enumerate() {
        let directory = format!("case-{index}");
        let archives = release_set(&fixture, &directory)?;
        rewrite_signature(&archives[3], edit)?;
        assert_eq!(
            code(collect(&fixture.root.join(&directory), &fixture.root)),
            Some(expected),
            "{index}"
        );
        assert!(!fixture.root.join(&directory).join("SHA256SUMS").exists());
    }

    let archives = release_set(&fixture, "missing")?;
    fs::remove_file(
        archives[0]
            .parent()
            .ok_or("parent")?
            .join(format!("signature-{}.json", TARGETS[0])),
    )?;
    assert!(collect(&fixture.root.join("missing"), &fixture.root).is_err());
    Ok(())
}
