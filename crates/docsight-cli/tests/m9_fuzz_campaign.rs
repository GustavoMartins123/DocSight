use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const TARGETS: [&str; 14] = [
    "fuzz_opc_container",
    "fuzz_ooxml_relationships",
    "fuzz_styles_cascade",
    "fuzz_numbering",
    "fuzz_table_grid",
    "fuzz_layout_paragraph",
    "fuzz_pdf_syntax",
    "fuzz_pdf_xref",
    "fuzz_pdf_content_stream",
    "fuzz_pdf_span_cluster",
    "fuzz_spatial_dql_parser",
    "fuzz_hit_test",
    "fuzz_evidence_bundle_manifest",
    "fuzz_cache_entry",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(fs::read_to_string(path)?)
}

#[test]
fn m9_declares_every_specified_fuzz_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = root();
    let manifest = read(&root.join("fuzz/Cargo.toml"))?;
    let declared = manifest
        .lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter_map(|name| name.strip_suffix('"'))
        .filter(|name| name.starts_with("fuzz_"))
        .collect::<Vec<_>>();
    let expected = TARGETS.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(declared.len(), TARGETS.len());
    assert_eq!(declared.into_iter().collect::<BTreeSet<_>>(), expected);

    for target in TARGETS {
        assert!(
            root.join(format!("fuzz/fuzz_targets/{target}.rs"))
                .is_file()
        );
    }
    Ok(())
}

#[test]
fn m9_keeps_a_seed_corpus_for_every_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = root();
    for target in TARGETS {
        let corpus = root.join(format!("fuzz/corpus/{target}"));
        let entries = fs::read_dir(&corpus)?.collect::<Result<Vec<_>, _>>()?;
        assert!(!entries.is_empty(), "empty corpus for {target}");
        assert!(entries.iter().all(|entry| entry.path().is_file()));
        assert!(
            entries
                .iter()
                .all(|entry| entry.metadata().is_ok_and(|metadata| metadata.len() > 0))
        );
    }
    Ok(())
}

#[test]
fn m9_ci_builds_and_campaigns_every_target() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = read(&root().join(".github/workflows/fuzz.yml"))?;
    assert!(workflow.contains("toolchain: nightly-2026-09-09"));
    assert!(workflow.contains("cargo-fuzz --locked --version 0.13.2"));
    assert!(workflow.contains("cargo +nightly-2026-09-09 fuzz build"));
    assert!(workflow.contains("cargo +nightly-2026-09-09 fuzz run ${{ matrix.target }}"));
    assert!(workflow.contains("-max_total_time=300"));
    assert!(workflow.contains("-max_len=1048576"));
    assert!(workflow.contains("-rss_limit_mb=1024"));
    assert!(workflow.contains("-timeout=10"));
    assert!(workflow.contains("uses: actions/upload-artifact@"));
    assert!(workflow.contains("fuzz-${{ matrix.target }}-artifacts"));
    for target in TARGETS {
        assert!(workflow.contains(&format!("- {target}")));
    }
    Ok(())
}
