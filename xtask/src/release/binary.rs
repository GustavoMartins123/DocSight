use crate::tooling::common::*;
use std::path::Path;

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| ToolError::new("INVALID_BINARY", "Executable offset overflow"))?;
    let data: [u8; 2] = bytes
        .get(offset..end)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| ToolError::new("INVALID_BINARY", "Executable header is truncated"))?;
    Ok(u16::from_le_bytes(data))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| ToolError::new("INVALID_BINARY", "Executable offset overflow"))?;
    let data: [u8; 4] = bytes
        .get(offset..end)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| ToolError::new("INVALID_BINARY", "Executable header is truncated"))?;
    Ok(u32::from_le_bytes(data))
}

pub fn check_bytes(bytes: &[u8], target: &str) -> Result<()> {
    super::checked_target(target)?;
    require(
        (64..=268_435_456).contains(&bytes.len()),
        "INVALID_BINARY",
        "Executable size is outside its bounds",
    )?;
    let valid = match target {
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => {
            let machine = if target.starts_with("x86_64") {
                62
            } else {
                183
            };
            bytes.starts_with(b"\x7fELF\x02\x01")
                && matches!(u16_at(bytes, 16)?, 2 | 3)
                && u16_at(bytes, 18)? == machine
        }
        "x86_64-apple-darwin" | "aarch64-apple-darwin" => {
            let machine = if target.starts_with("x86_64") {
                0x0100_0007
            } else {
                0x0100_000C
            };
            bytes.starts_with(b"\xcf\xfa\xed\xfe")
                && u32_at(bytes, 4)? == machine
                && u32_at(bytes, 12)? == 2
        }
        "x86_64-pc-windows-msvc" => {
            let offset = usize::try_from(u32_at(bytes, 60)?)
                .map_err(|_| ToolError::new("INVALID_BINARY", "PE header offset overflow"))?;
            require(
                (64..=4_194_304).contains(&offset),
                "INVALID_BINARY",
                "PE header offset exceeds its bounds",
            )?;
            bytes.starts_with(b"MZ")
                && bytes.get(offset..offset + 4) == Some(b"PE\0\0")
                && u16_at(bytes, offset + 4)? == 0x8664
                && u16_at(bytes, offset + 24)? == 0x20B
                && u16_at(bytes, offset + 22)? & 0x2000 == 0
        }
        _ => false,
    };
    require(
        valid,
        "BINARY_TARGET_MISMATCH",
        "Executable format or architecture does not match the target",
    )
}

pub fn check_binary(path: &Path, target: &str) -> Result<()> {
    no_symlinks(path)?;
    check_bytes(&read_bytes(path, MAX_FILE_BYTES)?, target)
}
