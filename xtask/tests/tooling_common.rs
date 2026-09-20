#[allow(dead_code)]
mod support;

use serde_json::json;
use std::fs;
use support::*;
use xtask::tooling::common::*;

#[test]
fn strict_json_rejects_duplicate_keys_trailing_data_and_non_finite_numbers() {
    assert_eq!(
        code(parse_json(br#"{"a":1,"a":2}"#)),
        Some("DUPLICATE_JSON_KEY")
    );
    assert_eq!(
        code(parse_json(br#"{"nested":{"b":true,"b":false}}"#)),
        Some("DUPLICATE_JSON_KEY")
    );
    assert_eq!(code(parse_json(br#"{"a":1} {}"#)), Some("INVALID_JSON"));
    assert_eq!(code(parse_json(br#"{"a":1e999}"#)), Some("INVALID_JSON"));
    assert_eq!(code(parse_json(br#"{"a":NaN}"#)), Some("INVALID_JSON"));
    let nested = format!("{}{}", "[".repeat(1024), "]".repeat(1024));
    assert_eq!(code(parse_json(nested.as_bytes())), Some("INVALID_JSON"));
}

#[test]
fn strict_json_preserves_valid_documents() -> TestResult {
    let value = parse_json(b"{\"text\":\"caf\\u00e9\",\"items\":[1,-2,3.5,null,false]}")?;
    assert_eq!(
        value,
        json!({"text": "café", "items": [1, -2, 3.5, null, false]})
    );
    Ok(())
}

#[test]
fn canonical_json_is_ascii_sorted_and_newline_terminated() -> TestResult {
    let bytes = json_bytes(&json!({"zeta": "café\u{7f}", "alpha": "😀"}))?;
    let text = String::from_utf8(bytes.clone())?;
    assert!(text.is_ascii());
    assert!(text.ends_with('\n'));
    assert!(text.find("alpha") < text.find("zeta"));
    assert!(text.contains("caf\\u00e9\\u007f"));
    assert!(text.contains("\\ud83d\\ude00"));
    assert_eq!(parse_json(&bytes)?["alpha"], "😀");
    assert_eq!(
        bytes,
        json_bytes(&json!({"alpha": "😀", "zeta": "café\u{7f}"}))?
    );
    Ok(())
}

#[test]
fn exclusive_publication_never_overwrites_existing_evidence() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("receipt.json");
    write_new(&path, b"first", false)?;
    assert_eq!(
        code(write_new(&path, b"second", false)),
        Some("OUTPUT_EXISTS")
    );
    assert_eq!(fs::read(&path)?, b"first");
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn published_files_receive_explicit_permissions() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir()?;
    let data = directory.path().join("data.json");
    let tool = directory.path().join("tool");
    write_new(&data, b"{}", false)?;
    write_new(&tool, b"binary", true)?;
    assert_eq!(fs::metadata(data)?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(tool)?.permissions().mode() & 0o777, 0o755);
    Ok(())
}

#[test]
fn bounded_reads_accept_the_limit_and_reject_one_byte_more() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("input.bin");
    fs::write(&path, vec![7u8; 64])?;
    assert_eq!(read_bytes(&path, 64)?.len(), 64);
    assert_eq!(code(read_bytes(&path, 63)), Some("FILE_SIZE_LIMIT"));
    assert_eq!(code(read_bytes(directory.path(), 64)), Some("INVALID_FILE"));
    Ok(())
}

#[test]
fn digests_match_published_sha256_vectors_in_memory_and_on_disk() -> TestResult {
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(digest(b"abc"), expected);
    assert_eq!(lowercase_hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("abc.txt");
    fs::write(&path, b"abc")?;
    assert_eq!(sha256_file(&path)?, expected);
    assert!(is_hex(expected, 64));
    assert!(checked_digest(&expected.to_uppercase()).is_err());
    Ok(())
}

#[test]
fn portable_member_paths_reject_traversal_and_reserved_names() {
    for valid in [
        "README.md",
        "schemas/v2/agent-envelope.json",
        "examples/a-b_c.pdf",
    ] {
        assert!(safe_member(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "",
        "/absolute",
        "../escape",
        "a/../b",
        "a//b",
        "./a",
        "trailing.",
        "back\\slash",
        "space name",
        "CON",
        "nul.txt",
        "com1.log",
        "LPT9",
    ] {
        assert_eq!(
            code(safe_member(invalid)),
            Some("UNSAFE_ARCHIVE_PATH"),
            "{invalid}"
        );
    }
    assert!(safe_member("COM0").is_ok());
    assert!(safe_member("console.md").is_ok());
}

#[test]
fn versions_and_revisions_are_canonical() {
    for valid in ["0.1.4", "1.20.300", "1.0.0-rc.1", "2.0.0-beta-2"] {
        assert!(checked_version(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "01.0.0",
        "1.0",
        "v1.0.0",
        "1.0.0-",
        "1.0.0-rc..1",
        "1.0.0+build",
    ] {
        assert!(checked_version(invalid).is_err(), "{invalid}");
    }
    assert!(checked_revision(REVISION).is_ok());
    assert!(checked_revision(&REVISION.to_uppercase()).is_err());
    assert!(checked_revision(&REVISION[..39]).is_err());
}

#[test]
fn contained_files_must_stay_inside_their_root() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    fs::create_dir(root.join("nested"))?;
    fs::write(root.join("nested/file.txt"), b"content")?;
    assert_eq!(
        contained_file(&root, "nested/file.txt")?,
        root.join("nested/file.txt")
    );
    assert!(contained_file(&root, "nested").is_err());
    assert!(contained_file(&root, "../file.txt").is_err());
    let probed = root.join("nested/../nested");
    #[cfg(unix)]
    assert_eq!(code(no_symlinks(&probed)), Some("INVALID_PATH"));
    #[cfg(windows)]
    {
        no_symlinks(&probed)?;
        let survivor = probed.canonicalize()?;
        assert!(
            survivor.starts_with(&root),
            "resolved traversal escaped its root"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_are_rejected_before_they_are_followed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), b"outside")?;
    std::os::unix::fs::symlink(outside.path(), root.join("link"))?;
    assert_eq!(
        code(contained_file(&root, "link/secret.txt")),
        Some("INVALID_FILE")
    );
    assert_eq!(code(no_symlinks(&root.join("link"))), Some("INVALID_FILE"));
    assert_eq!(code(list_files(&root, 16)), Some("INVALID_FILE"));
    Ok(())
}

#[test]
fn directory_traversal_is_bounded_and_sorted() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    fs::create_dir(root.join("b"))?;
    fs::write(root.join("b/2.json"), b"{}")?;
    fs::write(root.join("a.json"), b"{}")?;
    fs::write(root.join("c.txt"), b"")?;
    assert_eq!(
        list_files(&root, 4)?,
        vec![
            root.join("a.json"),
            root.join("b/2.json"),
            root.join("c.txt")
        ]
    );
    assert_eq!(code(list_files(&root, 3)), Some("RESOURCE_LIMIT"));
    assert_eq!(flat_files(&root, "json", 16)?, vec![root.join("a.json")]);
    assert_eq!(code(flat_files(&root, "json", 2)), Some("RESOURCE_LIMIT"));
    Ok(())
}

#[test]
fn field_helpers_enforce_exact_shapes_and_numeric_bounds() -> TestResult {
    let value = json!({"a": 1, "b": 2.5, "c": "text"});
    assert!(exact_keys(&value, &["a", "b", "c"]).is_ok());
    assert!(exact_keys(&value, &["a", "b"]).is_err());
    assert!(exact_keys(&value, &["a", "b", "d"]).is_err());
    assert_eq!(bounded_integer(field(&value, "a")?, 1, 1)?, 1);
    assert_eq!(
        code(bounded_integer(field(&value, "a")?, 2, 3)),
        Some("INVALID_INTEGER")
    );
    assert_eq!(
        code(bounded_integer(field(&value, "b")?, 0, 10)),
        Some("INVALID_INTEGER")
    );
    assert_eq!(finite_number(field(&value, "b")?, 0.0, 2.5)?, 2.5);
    assert_eq!(
        code(finite_number(field(&value, "b")?, 0.0, 2.0)),
        Some("INVALID_NUMBER")
    );
    assert_eq!(string(field(&value, "c")?)?, "text");
    assert_eq!(code(field(&value, "missing")), Some("INVALID_FIELDS"));
    assert!(is_code("UNSUPPORTED_FORMAT"));
    assert!(!is_code("unsupported"));
    assert!(!is_code("AB"));
    Ok(())
}
