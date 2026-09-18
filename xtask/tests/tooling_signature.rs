#[allow(dead_code)]
mod support;

use support::*;
use xtask::release::signature::{EmbeddedSignature, Inspection, inspect};

const LINUX: &str = "x86_64-unknown-linux-gnu";
const MAC: &str = "aarch64-apple-darwin";
const MAC_INTEL: &str = "x86_64-apple-darwin";
const WINDOWS: &str = "x86_64-pc-windows-msvc";
const ADHOC: u32 = 0x2;
const RUNTIME: u32 = 0x1_0000;
const LINKER_SIGNED: u32 = 0x2_0000;

fn embedded(bytes: &[u8], target: &str) -> TestResult<Inspection> {
    Ok(inspect(bytes, target)?)
}

#[test]
fn linux_executables_carry_no_platform_signature() -> TestResult {
    let inspection = embedded(&executable_header(LINUX), LINUX)?;
    assert_eq!(inspection.embedded, EmbeddedSignature::NotApplicable);
    assert_eq!(inspection.hardened_runtime, None);
    Ok(())
}

#[test]
fn mach_o_signatures_distinguish_absent_ad_hoc_and_developer_identities() -> TestResult {
    let unsigned = embedded(&executable_header(MAC), MAC)?;
    assert_eq!(unsigned.embedded, EmbeddedSignature::Absent);

    let linker = embedded(
        &signed_mach_o(MAC, &code_signature(ADHOC | LINKER_SIGNED, None)),
        MAC,
    )?;
    assert_eq!(linker.embedded, EmbeddedSignature::AdHoc);
    assert_eq!(linker.hardened_runtime, Some(false));

    let wrapped = embedded(&signed_mach_o(MAC, &code_signature(ADHOC, Some(0))), MAC)?;
    assert_eq!(wrapped.embedded, EmbeddedSignature::AdHoc);

    let developer = embedded(
        &signed_mach_o(MAC_INTEL, &code_signature(RUNTIME, Some(64))),
        MAC_INTEL,
    )?;
    assert_eq!(developer.embedded, EmbeddedSignature::Cms);
    assert_eq!(developer.hardened_runtime, Some(true));

    let without_runtime = embedded(&signed_mach_o(MAC, &code_signature(0, Some(64))), MAC)?;
    assert_eq!(without_runtime.embedded, EmbeddedSignature::Cms);
    assert_eq!(without_runtime.hardened_runtime, Some(false));
    Ok(())
}

#[test]
fn inconsistent_or_truncated_mach_o_signatures_are_rejected() -> TestResult {
    let contradictory = signed_mach_o(MAC, &code_signature(ADHOC, Some(64)));
    assert_eq!(
        code(inspect(&contradictory, MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let valid = code_signature(RUNTIME, Some(64));
    let mut truncated = signed_mach_o(MAC, &valid);
    truncated.pop();
    assert_eq!(
        code(inspect(&truncated, MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut foreign = valid.clone();
    foreign[0] = 0;
    assert_eq!(
        code(inspect(&signed_mach_o(MAC, &foreign), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut overlong = valid.clone();
    let length = u32::from_be_bytes(overlong[4..8].try_into()?);
    overlong[4..8].copy_from_slice(&(length + 1).to_be_bytes());
    assert_eq!(
        code(inspect(&signed_mach_o(MAC, &overlong), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut without_directory = valid;
    without_directory[12..16].copy_from_slice(&7u32.to_be_bytes());
    assert_eq!(
        code(inspect(&signed_mach_o(MAC, &without_directory), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut duplicated = signed_mach_o(MAC, &code_signature(RUNTIME, Some(64)));
    duplicated[16..20].copy_from_slice(&2u32.to_le_bytes());
    duplicated[20..24].copy_from_slice(&32u32.to_le_bytes());
    duplicated.copy_within(32..48, 48);
    assert_eq!(
        code(inspect(&duplicated, MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );
    Ok(())
}

#[test]
fn portable_executables_report_their_authenticode_certificate_table() -> TestResult {
    let unsigned = embedded(&executable_header(WINDOWS), WINDOWS)?;
    assert_eq!(unsigned.embedded, EmbeddedSignature::Absent);
    let empty_directory = embedded(&signed_portable_executable(None), WINDOWS)?;
    assert_eq!(empty_directory.embedded, EmbeddedSignature::Absent);
    let signed = embedded(&signed_portable_executable(Some((64, 0x0200, 2))), WINDOWS)?;
    assert_eq!(signed.embedded, EmbeddedSignature::Cms);
    assert_eq!(signed.hardened_runtime, None);
    let smallest = embedded(&signed_portable_executable(Some((9, 0x0200, 2))), WINDOWS)?;
    assert_eq!(smallest.embedded, EmbeddedSignature::Cms);
    Ok(())
}

#[test]
fn malformed_certificate_tables_are_rejected() -> TestResult {
    for certificate in [
        (65, 0x0200, 2),
        (8, 0x0200, 2),
        (64, 0x0100, 2),
        (64, 0x0200, 1),
    ] {
        assert_eq!(
            code(inspect(
                &signed_portable_executable(Some(certificate)),
                WINDOWS
            )),
            Some("MALFORMED_CODE_SIGNATURE"),
            "{certificate:?}"
        );
    }
    let mut beyond = signed_portable_executable(Some((64, 0x0200, 2)));
    beyond.truncate(575);
    assert_eq!(
        code(inspect(&beyond, WINDOWS)),
        Some("MALFORMED_CODE_SIGNATURE")
    );
    let mut outside_header = signed_portable_executable(Some((64, 0x0200, 2)));
    outside_header[148..150].copy_from_slice(&144u16.to_le_bytes());
    assert_eq!(
        code(inspect(&outside_header, WINDOWS)),
        Some("MALFORMED_CODE_SIGNATURE")
    );
    Ok(())
}

#[test]
fn inspection_rejects_an_executable_for_another_target() {
    assert_eq!(
        code(inspect(&executable_header(WINDOWS), MAC)),
        Some("BINARY_TARGET_MISMATCH")
    );
}
