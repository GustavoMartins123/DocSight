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
const SIGNATURE_AT: usize = 256;

fn put_le(bytes: &mut Vec<u8>, offset: usize, value: u32) {
    if bytes.len() < offset + 4 {
        bytes.resize(offset + 4, 0);
    }
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_be(bytes: &mut Vec<u8>, offset: usize, value: u32) {
    if bytes.len() < offset + 4 {
        bytes.resize(offset + 4, 0);
    }
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn superblob(flags: u32, cms: Option<usize>) -> Vec<u8> {
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

fn mach_o(target: &str, signature: &[u8]) -> Vec<u8> {
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

fn portable_executable(certificate: Option<(u32, u16, u16)>) -> Vec<u8> {
    let mut bytes = executable_header(WINDOWS);
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

    let linker = embedded(&mach_o(MAC, &superblob(ADHOC | LINKER_SIGNED, None)), MAC)?;
    assert_eq!(linker.embedded, EmbeddedSignature::AdHoc);
    assert_eq!(linker.hardened_runtime, Some(false));

    let wrapped = embedded(&mach_o(MAC, &superblob(ADHOC, Some(0))), MAC)?;
    assert_eq!(wrapped.embedded, EmbeddedSignature::AdHoc);

    let developer = embedded(&mach_o(MAC_INTEL, &superblob(RUNTIME, Some(64))), MAC_INTEL)?;
    assert_eq!(developer.embedded, EmbeddedSignature::Cms);
    assert_eq!(developer.hardened_runtime, Some(true));

    let without_runtime = embedded(&mach_o(MAC, &superblob(0, Some(64))), MAC)?;
    assert_eq!(without_runtime.embedded, EmbeddedSignature::Cms);
    assert_eq!(without_runtime.hardened_runtime, Some(false));
    Ok(())
}

#[test]
fn inconsistent_or_truncated_mach_o_signatures_are_rejected() -> TestResult {
    let contradictory = mach_o(MAC, &superblob(ADHOC, Some(64)));
    assert_eq!(
        code(inspect(&contradictory, MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let valid = superblob(RUNTIME, Some(64));
    let mut truncated = mach_o(MAC, &valid);
    truncated.pop();
    assert_eq!(
        code(inspect(&truncated, MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut foreign = valid.clone();
    foreign[0] = 0;
    assert_eq!(
        code(inspect(&mach_o(MAC, &foreign), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut overlong = valid.clone();
    let length = u32::from_be_bytes(overlong[4..8].try_into()?);
    overlong[4..8].copy_from_slice(&(length + 1).to_be_bytes());
    assert_eq!(
        code(inspect(&mach_o(MAC, &overlong), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut without_directory = valid;
    without_directory[12..16].copy_from_slice(&7u32.to_be_bytes());
    assert_eq!(
        code(inspect(&mach_o(MAC, &without_directory), MAC)),
        Some("MALFORMED_CODE_SIGNATURE")
    );

    let mut duplicated = mach_o(MAC, &superblob(RUNTIME, Some(64)));
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
    let empty_directory = embedded(&portable_executable(None), WINDOWS)?;
    assert_eq!(empty_directory.embedded, EmbeddedSignature::Absent);
    let signed = embedded(&portable_executable(Some((64, 0x0200, 2))), WINDOWS)?;
    assert_eq!(signed.embedded, EmbeddedSignature::Cms);
    assert_eq!(signed.hardened_runtime, None);
    let smallest = embedded(&portable_executable(Some((9, 0x0200, 2))), WINDOWS)?;
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
            code(inspect(&portable_executable(Some(certificate)), WINDOWS)),
            Some("MALFORMED_CODE_SIGNATURE"),
            "{certificate:?}"
        );
    }
    let mut beyond = portable_executable(Some((64, 0x0200, 2)));
    beyond.truncate(575);
    assert_eq!(
        code(inspect(&beyond, WINDOWS)),
        Some("MALFORMED_CODE_SIGNATURE")
    );
    let mut outside_header = portable_executable(Some((64, 0x0200, 2)));
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
