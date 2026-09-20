# Backlog

Work that is known, deliberately deferred, and classified by whether it blocks the first serious release.

`PRODUCT_SCOPE.md` states what DocSight promises today. This file states what it does not promise yet and when that is expected to change. Nothing here is a commitment to a date; the classification is about ordering, not scheduling.

Classification:

- **v1** — required before DocSight can be installed and trusted by a third party. A gap that makes a common real document unusable, or that makes an existing promise unreliable.
- **post-v1** — real value, but the product is honest and useful without it.
- **experimental** — unproven direction. Needs a use case, a contract and a test strategy before any code.

---

## v1

### Operability

No open blocking items.

### Evidence that requires people

Tooling can prepare these, but only people can produce them. None exists yet.

| Item | Current state |
| --- | --- |
| Reviewed ground truth for each document class | `release/ground-truth` holds no records, so every class rests on engine consistency or on corpus expectations only. `quality prepare` proposes records; a reviewer must check and sign them off as described in RELEASE.md. |
| Consented real documents per class | The tracked corpus is synthetic. `docx-text`, `docx-multi-section`, `pdf-text` and `pdf-visual` have no documents at all. |
| Approval of the readiness thresholds | `release/readiness-policy.json` records the 100-document and 25-real-per-format thresholds as `engineering-proposal`; approving them takes a reviewed commit and an approved `policy` review. |

### Pending DS18 measurements and guards

Recorded 2026-09-20. The DS18 operation matrix, budgets and cost structure in `benchmarks/ds18-operations.json` and `PERFORMANCE.md` were measured on Windows x64 release only.

| Item | Current state |
| --- | --- |
| Linux x86_64 operation numbers and peak-memory budgets | `max_peak_memory_bytes` is null everywhere and wall ceilings carry a Windows reference. Measuring on the canonical Linux host, filling the memory budgets and confirming the Windows ceilings still hold is pending maintainer action. |
| Line-ending guard for byte-asserted files | `schemas/v2/*.json` digests are asserted byte for byte by the m15 conformance suite, but a stale CRLF checkout on Windows silently changes those bytes. `.gitattributes` already mandates `eol=lf`; a `rust-only` gate that rejects CRLF in tracked text files is designed but not implemented. |

---

## post-v1

- Real font discovery, loading and shaping instead of the deterministic proportional fallback, with a declared font fingerprint in the reproducibility inputs.
- Filtered image scaling. Figure scaling is nearest-neighbour, chosen for determinism; a box or Lanczos filter would look closer to Word at the cost of a defined tolerance in the visual goldens.
- Remaining PNG variants: 1, 2, 4 and 16 bits per channel, and interlaced images. Each currently fails closed with a diagnostic naming the variant.
- Shading (`sh`) painting. The operator no longer vetoes a document, but the shaded area is left unpainted and reported through `PDF_SHADING_UNSUPPORTED`.
- Image XObject decoding. PDF image pixels are still placeholders; DOCX PNG and JPEG parts are already decoded.
- Contextual spacing (`w:contextualSpacing`), which is parsed and reported but not applied between paragraphs of the same style.
- Annotation appearance streams: an annotation currently contributes geometry and text to the IR, not its rendered pixels.
- `Watermark` overlays in the IR. The entity exists but no parser produces it.
- DOCX floating objects and shapes. Recognized DrawingML lines are preserved as `Shape` blocks, but their position, extent, stroke and pixels are not projected (`DOCX_SHAPE_VISUAL_OMITTED`); other Office shapes are not produced.
- Row-level table pagination. Paragraphs split by line across pages, but a table that does not fit the remaining space moves to the next page as a whole (`DOCX_PAGINATION_BLOCK_GRANULAR`).
- Multi-column sections. A section with more than one text column is laid out as one column (`DOCX_SECTION_COLUMNS_UNSUPPORTED`), and a `nextColumn` section break starts a new page (`DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED`).
- Header and footer formatting. Header and footer parts are projected as deterministic 9 pt plain text lines without their own run formatting, tab stops, tables or drawings (`DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED`), and body text does not move away from a tall header or footer (`DOCX_HEADER_FOOTER_OVERLAPS_BODY`).
- Binding gutters, which are parsed but not added to the body margins (`DOCX_SECTION_GUTTER_IGNORED`).
- Page parity with restarted numbering. An `evenPage` or `oddPage` section break decides parity by physical page number, while even-page headers and footers follow the displayed page number; the two can disagree when a section restarts its numbering.
- Per-revision tracked changes: authorship, timestamp and content, rather than insertion and deletion counts.
- Complex DOCX numbering: multi-level list restarts, custom patterns and style-linked numbering.
- Table columns, column spans and text flow across columns.
- PDF caption reconstruction, which would let the `caption` DQL selector work for PDF instead of failing closed.
- Table detection quality: calibrated confidence for unruled and partially-ruled tables, and a second detector with declared provenance.
- Richer `diff` lineage across versions, reducing `DIFF_LINEAGE_AMBIGUOUS`.
- Caching beyond the document IR: raster output, crops, traces, proof bundles and diff results are recomputed on every invocation.
- Encrypted cache entries, which would allow caching the IR of password-protected PDFs and protect cached content at rest.
- Graded quality metrics: text similarity instead of digest equality, pixel-tolerance render comparison instead of PNG digest equality, and geometry and render measured beyond page one.
- Quality measurement of real documents from private registers across all five native targets, with per-class comparisons between release candidates.
- Distribution authentication: signing and macOS notarization, with maintainer credentials and clean-machine validation. Native package generation, checksums, installation instructions and a five-target validation workflow are implemented; their existence does not prove a release has been built or published.

---

## experimental

Nothing here is planned. Each item requires a written use case, a stable contract and a test strategy before implementation, per Rule 1 of the implementation plan.

- DQL beyond the current constrained selector grammar: joins, aggregation, user-defined predicates.
- A plugin interface for third-party detectors or exporters.
- Incremental or streaming ingestion of very large documents.
- A local viewer or TUI over the IR.
- Structured export targets beyond the current ones, such as a normalized archival format.

Items explicitly rejected rather than deferred are listed under *Out of scope* in `PRODUCT_SCOPE.md`.


## Reconciled implemented items

Incremental PDF cross-reference updates (`Prev` chains, formerly A1b) are already
implemented in `crates/docsight-pdf/src/syntax.rs`. The existing
`reads_cross_reference_streams_with_compressed_objects` integration test in
`crates/docsight-pdf/tests/pdf.rs` includes an `incremental_update: true` case.
This corrects the stale backlog classification; it is not a new parser
implementation or a claim that the native test was rerun in every environment.

Multi-section DOCX geometry and line-level pagination (DS13) are implemented in
`crates/docsight-layout` on Document IR schema 1.3. Each section applies its own
page size, orientation, margins, start type, header and footer variants and page
numbering; paragraphs split by line across pages with keep-with-next,
keep-lines and widow/orphan control, and a paragraph that continues on later
pages keeps one identifier with page-local continuation geometry. The
behaviour is covered by `crates/docsight-layout/tests/pagination.rs`,
`crates/docsight-ooxml/tests/sections.rs` and the CLI contract tests. Both items
are recorded as resolved in `release/known-gaps.json`; the remaining layout
limitations are listed under post-v1 above with their diagnostics.

DS10 candidate tooling is described in RELEASE.md and DS11 observation tooling
in BETA.md. The machine-readable `release/known-gaps.json` retains the open V1
engine gap above. A completed tooling implementation is not completion of the
native validation, real beta or V1 acceptance criteria.

Per-invocation overrides for ingestion limits (`--max-document-bytes`) are
implemented in `crates/docsight-cli` and `crates/docsight-core`. Callers can
override the default 64MB inspection limit per invocation (e.g.
`--max-document-bytes 128mb`), propagating through sandbox workers and cache
handoffs while enforcing `RESOURCE_LIMIT` (exit code 13) when exceeded.
Covered by `crates/docsight-cli/tests/m9_hardening_cli.rs`.
