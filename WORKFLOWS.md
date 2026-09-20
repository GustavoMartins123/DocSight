# DocSight workflows

This guide turns isolated commands into complete, recognizable journeys. Every workflow below runs with the public CLI surface only: human-readable output by default, canonical `--agent` JSON for machines, NDJSON streaming with `--ndjson` for large collections. Machine callers must not parse human output; human readers must not treat JSON as prose.

All examples assume a local `docsight` binary and offline documents. Nothing is fetched, executed or uploaded. Untrusted input should add `--sandbox` to every document command.

The same journeys exist as executable manifests in `fixtures/tasks/` (`workflow-*.json`). Each manifest declares its documents, invocation and byte budgets, chained steps and required evidence, and is executed end to end by `crates/docsight-cli/tests/task_scenarios_cli.rs` using only the `docsight` executable, and by `cargo xtask task run --scenario FILE --root fixtures/validation --engine ./target/release/docsight --out receipt.json`, which records a `docsight.task-receipt/v1` receipt.

## Inspect a document

Goal: go from a file to structure, text and tables.

```bash
docsight inspect report.docx
docsight outline report.docx
docsight text report.docx --max-items 20 --text-limit 2000
docsight tables report.docx
docsight fingerprint report.docx
```

Machine form: prepend `--agent` to every command. Bound large outputs before parsing them: `--max-items`, `--text-limit`, `--max-bytes` and `--budget` (for example `--budget 8kb`). A continuation token in the response retrieves the remainder; tokens are bound to the command, document and query that produced them.

See `fixtures/tasks/workflow-inspect-docx.json`.

## Compare two revisions

Goal: classify what changed, confirm identity, keep visual evidence.

```bash
docsight diff before.docx after.docx
docsight diff report.docx report.docx
docsight render report.docx --page 1 --dpi 36 --out page1.png
```

A self diff reports zero semantic changes; repeated identical objects are paired in document order instead of reported as removed and added. A changed record that rests on low-confidence reconstruction is explicitly non-authoritative: read `evidence` before treating it as proof of a source-level change. With `--visual --out-dir dir`, each page also exposes a deterministic absolute-difference PNG with its digest.

See `fixtures/tasks/workflow-compare-revisions.json`.

## Verify support and fidelity

Goal: answer how much of the document was actually interpreted.

```bash
docsight inspect report.docx
docsight coverage report.docx
docsight evidence report.docx tbl_0008
docsight fingerprint report.docx
```

`coverage` reports text, structure, geometry, visual and resource fidelity per page and globally, plus `unsupported_feature_count`. Every limitation in `PRODUCT_SCOPE.md` surfaces at runtime as a typed diagnostic with its consequence; `inspect`, `coverage` and `evidence` derive fidelity from the same catalogue and cannot disagree. `fingerprint` binds the exact bytes, engine, schemas and layout profile a result depends on.

See `fixtures/tasks/workflow-verify-support.json`.

## Investigate a region

Goal: find every occurrence, expand its context and prove it visually.

```bash
docsight find report.pdf "invoice number" --ignore-case
docsight context report.pdf cell_0007 --include content,geometry
docsight crop report.pdf --object cell_0007 --dpi 36 --out region.png
```

`find` searches at the finest granularity the IR holds (a table is searched cell by cell) and returns the matched range with surrounding context. Every identifier it emits is accepted directly by `evidence`, `context` and `crop`. Crop regions derive from the object bounding box; a region that continues on another page reports it instead of guessing.

See `fixtures/tasks/workflow-investigate-pdf.json`.

## Reproduce offline

Goal: hand one object or region to someone else with everything needed to verify it without the original file.

```bash
docsight bundle report.docx --object tbl_0008 --include-crop --out evidence.dse
docsight verify evidence.dse
docsight render report.docx --page 2 --trace page2.dstrace --out page2.png
docsight replay page2.dstrace --verify
```

A `.dse` bundle is content-addressed and self-contained; `verify` replays its embedded trace and recomputes evidence from the embedded source. A `.dstrace` records the source digest, reproduction fingerprint, display list and raster fingerprint; `replay --verify` fails closed with `VERIFICATION_FAILED` on any divergence. Unknown entries, compression, excess sizes and non-canonical archives fail closed.

See `fixtures/tasks/workflow-reproduce-bundle.json`.

## Errors

Failures are typed and decidable without string matching. In `--agent` mode the envelope on stderr carries a stable `code` with its process exit code, for example `UNSUPPORTED_FORMAT` with exit 10 for an unidentified document, `ENCRYPTED` with exit 12 for an unknown password, or `RESOURCE_LIMIT` with exit 13 at a published ingestion boundary. Successful responses keep stderr empty. See `fixtures/tasks/diagnose-unsupported.json` for the shape.

## Budgets and determinism

The same bytes, engine, backends, fonts and options produce the same bytes: repeated runs are byte-identical apart from caller-chosen artifact paths. Keep each workflow inside explicit budgets (`--max-items`, `--text-limit`, `--budget`, `--max-bytes`) and persist continuation tokens from NDJSON `done` records. The manifests above declare the budgets each journey needs; exceeding them fails closed instead of truncating silently.
