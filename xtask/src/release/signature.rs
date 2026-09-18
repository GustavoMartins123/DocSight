use super::binary::check_bytes;
use crate::tooling::common::*;
use serde::{Deserialize, Serialize};

const LC_CODE_SIGNATURE: u32 = 0x1d;
const MAX_LOAD_COMMANDS: u32 = 4096;
const SUPERBLOB_MAGIC: u32 = 0xfade_0cc0;
const CODE_DIRECTORY_MAGIC: u32 = 0xfade_0c02;
const BLOB_WRAPPER_MAGIC: u32 = 0xfade_0b01;
const MAX_SIGNATURE_SLOTS: u32 = 64;
const CODE_DIRECTORY_SLOT: u32 = 0;
const SIGNATURE_SLOT: u32 = 0x1_0000;
const ADHOC_FLAG: u32 = 0x2;
const RUNTIME_FLAG: u32 = 0x1_0000;
const CERTIFICATE_DIRECTORY: u32 = 4;
const CERTIFICATE_REVISION: u16 = 0x0200;
const PKCS_SIGNED_DATA: u16 = 0x0002;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddedSignature {
    NotApplicable,
    Absent,
    AdHoc,
    Cms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub embedded: EmbeddedSignature,
    #[serde(deserialize_with = "required_option")]
    pub hardened_runtime: Option<bool>,
}

fn malformed() -> ToolError {
    ToolError::new(
        "MALFORMED_CODE_SIGNATURE",
        "Executable code signature structure is truncated or inconsistent",
    )
}

fn window(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    let end = offset.checked_add(length).ok_or_else(malformed)?;
    bytes.get(offset..end).ok_or_else(malformed)
}

fn little16(bytes: &[u8], offset: usize) -> Result<u16> {
    let data: [u8; 2] = window(bytes, offset, 2)?
        .try_into()
        .map_err(|_| malformed())?;
    Ok(u16::from_le_bytes(data))
}

fn little32(bytes: &[u8], offset: usize) -> Result<u32> {
    let data: [u8; 4] = window(bytes, offset, 4)?
        .try_into()
        .map_err(|_| malformed())?;
    Ok(u32::from_le_bytes(data))
}

fn big32(bytes: &[u8], offset: usize) -> Result<u32> {
    let data: [u8; 4] = window(bytes, offset, 4)?
        .try_into()
        .map_err(|_| malformed())?;
    Ok(u32::from_be_bytes(data))
}

fn index(value: u32) -> Result<usize> {
    usize::try_from(value).map_err(|_| malformed())
}

pub fn inspect(bytes: &[u8], target: &str) -> Result<Inspection> {
    check_bytes(bytes, target)?;
    if target.ends_with("-linux-gnu") {
        Ok(Inspection {
            embedded: EmbeddedSignature::NotApplicable,
            hardened_runtime: None,
        })
    } else if target.ends_with("-apple-darwin") {
        mach_o(bytes)
    } else {
        portable_executable(bytes)
    }
}

fn code_signature_command(bytes: &[u8]) -> Result<Option<(usize, usize)>> {
    let count = little32(bytes, 16)?;
    require(
        count <= MAX_LOAD_COMMANDS,
        "MALFORMED_CODE_SIGNATURE",
        "Executable declares too many load commands",
    )?;
    let end = 32usize
        .checked_add(index(little32(bytes, 20)?)?)
        .ok_or_else(malformed)?;
    require(
        end <= bytes.len(),
        "MALFORMED_CODE_SIGNATURE",
        "Load commands exceed the executable",
    )?;
    let mut offset = 32usize;
    let mut found = None;
    for _ in 0..count {
        let command = little32(bytes, offset)?;
        let length = index(little32(bytes, offset + 4)?)?;
        let next = offset.checked_add(length).ok_or_else(malformed)?;
        require(
            length >= 8 && length % 8 == 0 && next <= end,
            "MALFORMED_CODE_SIGNATURE",
            "Load command size is outside the load command area",
        )?;
        if command == LC_CODE_SIGNATURE {
            require(
                found.is_none() && length == 16,
                "MALFORMED_CODE_SIGNATURE",
                "Executable must declare at most one code signature command",
            )?;
            found = Some((
                index(little32(bytes, offset + 8)?)?,
                index(little32(bytes, offset + 12)?)?,
            ));
        }
        offset = next;
    }
    Ok(found)
}

fn mach_o(bytes: &[u8]) -> Result<Inspection> {
    let Some((offset, size)) = code_signature_command(bytes)? else {
        return Ok(Inspection {
            embedded: EmbeddedSignature::Absent,
            hardened_runtime: None,
        });
    };
    let data = window(bytes, offset, size)?;
    require(
        big32(data, 0)? == SUPERBLOB_MAGIC,
        "MALFORMED_CODE_SIGNATURE",
        "Code signature does not start with a signature superblob",
    )?;
    let length = index(big32(data, 4)?)?;
    require(
        (12..=data.len()).contains(&length),
        "MALFORMED_CODE_SIGNATURE",
        "Signature superblob length exceeds its data",
    )?;
    let superblob = &data[..length];
    let slots = big32(superblob, 8)?;
    require(
        slots <= MAX_SIGNATURE_SLOTS,
        "MALFORMED_CODE_SIGNATURE",
        "Signature superblob declares too many slots",
    )?;
    let mut directory_flags = None;
    let mut cms = None;
    for slot in 0..index(slots)? {
        let entry = 12 + slot * 8;
        let kind = big32(superblob, entry)?;
        let start = index(big32(superblob, entry + 4)?)?;
        let magic = big32(superblob, start)?;
        let blob_length = index(big32(superblob, start + 4)?)?;
        window(superblob, start, blob_length)?;
        require(
            blob_length >= 8,
            "MALFORMED_CODE_SIGNATURE",
            "Signature blob is shorter than its header",
        )?;
        match kind {
            CODE_DIRECTORY_SLOT => {
                require(
                    magic == CODE_DIRECTORY_MAGIC && blob_length >= 16 && directory_flags.is_none(),
                    "MALFORMED_CODE_SIGNATURE",
                    "Code directory slot does not hold one code directory",
                )?;
                directory_flags = Some(big32(superblob, start + 12)?);
            }
            SIGNATURE_SLOT => {
                require(
                    magic == BLOB_WRAPPER_MAGIC && cms.is_none(),
                    "MALFORMED_CODE_SIGNATURE",
                    "Signature slot does not hold one signature wrapper",
                )?;
                cms = Some(blob_length > 8);
            }
            _ => {}
        }
    }
    let flags = directory_flags.ok_or_else(malformed)?;
    let adhoc = flags & ADHOC_FLAG != 0;
    let embedded = match (cms.unwrap_or(false), adhoc) {
        (true, false) => EmbeddedSignature::Cms,
        (false, _) => EmbeddedSignature::AdHoc,
        (true, true) => return Err(malformed()),
    };
    Ok(Inspection {
        embedded,
        hardened_runtime: Some(flags & RUNTIME_FLAG != 0),
    })
}

fn portable_executable(bytes: &[u8]) -> Result<Inspection> {
    let absent = Inspection {
        embedded: EmbeddedSignature::Absent,
        hardened_runtime: None,
    };
    let header = index(little32(bytes, 60)?)?;
    let optional = header.checked_add(24).ok_or_else(malformed)?;
    let optional_size = usize::from(little16(bytes, header + 20)?);
    let directories = little32(bytes, optional + 108)?;
    if directories <= CERTIFICATE_DIRECTORY {
        return Ok(absent);
    }
    let entry = optional + 112 + index(CERTIFICATE_DIRECTORY)? * 8;
    require(
        entry + 8 <= optional + optional_size,
        "MALFORMED_CODE_SIGNATURE",
        "Certificate directory lies outside the optional header",
    )?;
    let offset = index(little32(bytes, entry)?)?;
    let size = index(little32(bytes, entry + 4)?)?;
    if offset == 0 && size == 0 {
        return Ok(absent);
    }
    let table = window(bytes, offset, size)?;
    let length = index(little32(table, 0)?)?;
    require(
        (9..=size).contains(&length)
            && little16(table, 4)? == CERTIFICATE_REVISION
            && little16(table, 6)? == PKCS_SIGNED_DATA,
        "MALFORMED_CODE_SIGNATURE",
        "Certificate table does not hold an Authenticode PKCS #7 signature",
    )?;
    Ok(Inspection {
        embedded: EmbeddedSignature::Cms,
        hardened_runtime: None,
    })
}
