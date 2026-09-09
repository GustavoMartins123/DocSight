# DOCSIGHT fuzzing

The fuzz workspace exercises the parser, layout, spatial-query, hit-testing, table-inference, and evidence-bundle production paths required by M9. Inputs are capped at 1 MiB by the harnesses so a single libFuzzer input cannot bypass the product's own resource-limit checks through unbounded harness allocation.

Install a nightly Rust toolchain and `cargo-fuzz`, then build every target:

```text
rustup toolchain install nightly-2026-09-09
cargo install cargo-fuzz --locked --version 0.13.2
cargo +nightly-2026-09-09 fuzz build
```

Run one target with its versioned corpus:

```text
cargo +nightly-2026-09-09 fuzz run fuzz_opc_container
```

The required targets are:

- `fuzz_opc_container`
- `fuzz_ooxml_relationships`
- `fuzz_styles_cascade`
- `fuzz_numbering`
- `fuzz_table_grid`
- `fuzz_layout_paragraph`
- `fuzz_pdf_syntax`
- `fuzz_pdf_xref`
- `fuzz_pdf_content_stream`
- `fuzz_pdf_span_cluster`
- `fuzz_spatial_dql_parser`
- `fuzz_hit_test`
- `fuzz_evidence_bundle_manifest`

Crashes are written under `fuzz/artifacts/` and remain untracked until they are minimized, understood, and converted into a permanent regression fixture. CI preserves them as a target-specific workflow artifact when a campaign fails. Corpus changes are reviewed like other test-data changes; do not replace or discard a crashing input merely to make a campaign pass.

Pushes and pull requests compile every target. The scheduled and manually dispatched workflow runs each target independently for five minutes, accepts generated inputs up to the harness limit of 1 MiB, enforces a ten-second per-input timeout, and caps resident memory at 1 GiB.
