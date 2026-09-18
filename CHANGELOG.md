# Changelog

## Unreleased

- Merge warnings that repeat the same code, severity, message, effect and page into one record with an `occurrences` count in JSON, NDJSON and human output, so a condition that affects many objects no longer floods the output. A two-column paper went from 899 warning records to 110.
- Pass the sandbox policy to the isolated worker so its CPU time and memory limits are the ones the caller configured rather than the defaults, enforce the 30-second CPU budget as CPU time on the worker process, and stop only a blocked worker at a separate 120-second wall-clock deadline, with a distinct message for each limit.
- Rasterize PDF strokes by testing each segment, join and cap only over the pixels it can reach instead of every pixel of the whole stroke. Output is byte-identical; a page whose table borders are long polylines rendered in about 0.04 s instead of 17 s.
- Accept a PDF whose `%PDF-` header follows only whitespace or NUL bytes within the first 1024 bytes, read its byte offsets relative to the header as PDF readers do, and report `PDF_HEADER_OFFSET`. Other leading bytes are still rejected.
- Read DOCX spacing, indentation, page size and margin lengths written as decimals within 0.001 of a whole number of twentieths of a point, such as `240.00000000000003`, as that whole number and report them once with `DOCX_MEASURE_ROUNDED`, instead of rejecting the document. Other non-integer lengths are still rejected.
- Pair identical headings, paragraphs and images that occur the same number of times in both documents in document order, so a document compared with itself reports no semantic changes and repeated content is no longer reported as removed and added. A change in the number of identical copies remains ambiguous.
- Accept PDF form XObjects whose resources list the form itself or an ancestor, including forms that inherit their parent's resources, and reject a form XObject cycle only when content actually invokes it.
- Classify every corpus case by document class and complexity using the versioned taxonomy in `release/document-classes.json`, and require the case format and expected outcome to agree with its class.
- Add `quality measure`, which measures each corpus document twice in the packaged engine and reports determinism, class signals, page-one raster coverage, empty self-diff and adversarial rejections, plus structure, text, geometry, diagnostics, render and diff agreement with ground truth, per document and per class, with the basis of every result and no extrapolation beyond the listed documents.
- Add `quality prepare` and `quality validate` for a ground truth register whose records stay `unreviewed` until a named reviewer binds a review note, and `quality compare` to list regressions between two measurements.
- Record the readiness thresholds as an engineering proposal and keep V1 readiness blocked until a reviewed policy approves them.
- Add adversarial corpus cases for broken PDF cross-reference tables, incomplete and truncated DOCX packages, invalid OOXML markup and XML nesting beyond the published depth limit.

- Add an opt-in content-addressed document IR cache with `--cache-dir`, `--cache-max-bytes` and `--cache-max-entries` for the eighteen commands that read the IR. Keys cover the document digest and format, the executable digest, engine and IR schema versions, layout profile and font fingerprint; output is byte-identical on a miss, a hit and without the cache.
- Write cache entries atomically without overwriting, validate every entry in full before reuse, quarantine invalid entries with a reason and parse the document again, evict least-recently-used entries at the byte and entry limits and remove stale temporary files.
- Keep the cache in the parent process under `--sandbox`: the worker receives only the key, a validated entry or a private result directory, and the parent validates the returned IR before storing it.
- Validate a document IR in full, including its canonical form, before writing it to the cache, verify integrity, key, document identity and IR version on every read, and verify the canonical form again for entries returned by a sandboxed worker and during `cache verify` and `cache prune`.
- Record whether a cache entry was produced in process or by a sandboxed worker and report both counts in `cache stats`.
- Quarantine an entry only while the file still holds the rejected bytes, so a concurrently published entry is never quarantined by mistake, and let the parent revalidate an entry its worker rejected instead of trusting the worker.
- Add a sandbox write-path probe and worker test, a `fuzz_cache_entry` fuzz target, and tests for interrupted processes and unwritable cache directories.
- Add the `cache stats|verify|prune|clear` command with the `cache-result.json` schema, the `cache` capability and the `CACHE_ENTRY_QUARANTINED` and `CACHE_RESULT_DISCARDED` diagnostics. `--cache-dir` is rejected with `--password-file` and for commands that do not read the IR.

- Lay out every DOCX section with its own page size, orientation, margins, start type, title page, page numbering and default, first-page and even-page headers and footers, and link each DOCX page to its section through `section_index`.
- Paginate DOCX paragraphs, headings, list items and notes by line with `keep-with-next`, `keep-lines` and widow/orphan control. A block that continues across pages keeps one identifier and exposes page-local `continuations`; `find`, `page`, `hit`, `focus`, `peek`, `resolve`, `context`, spatial queries and `crop` use the fragment on the relevant page.
- Resolve `PAGE`, `NUMPAGES` and `SECTIONPAGES` header and footer fields per page, including simple and complex fields, restarts and roman or letter formats.
- Advance the Document IR schema to 1.3 with additive section, page, layout flag and continuation fields, and add the optional `continuations` field to agent semantic objects and `continued` to page spans.
- Stop emitting `DOCX_SECTIONS_COLLAPSED`; emit `DOCX_PAGINATION_BLOCK_GRANULAR` only for a table that moves to the next page as a whole, and report the remaining section, header, footer and page-number limitations with dedicated diagnostics.
- Preserve DrawingML lines as `Shape` blocks with `DOCX_SHAPE_VISUAL_OMITTED` instead of rejecting the document.

- Replace all permanent DS9-DS12 interpreter tooling and auxiliary tests with native Rust modules and integration tests in xtask.
- Route release, notices, changelog, smoke, corpus, beta, validation and readiness through cargo xtask and guard the Rust-only architecture in CI.
- Require both docsight and docsight-worker in every native archive.
- Version native validation receipts as v2 with explicit pass, fail, unavailable, blocked and not_run states and nullable unavailable exit codes.

These changes implement candidate tooling; they do not claim a completed native
validation campaign, real beta, signed release or V1 acceptance.

- Gate five native targets on locked tests/builds, extracted-binary smoke checks and the synthetic corpus.
- Generate deterministic Git-range release notes and locked dependency license notices.
- Collect opt-in local beta metadata without document content, paths, passwords or automatic upload.
- Validate hash-pinned corpus cases, paired-document diffs, typed errors and repeated output bytes.
- Detect documents changed during the final corpus command, not only between repeats.
- Retain all validation gates and distinguish failed tests from unavailable compilers.
- Evaluate candidate-bound V1 evidence and preserve open engine gaps and required human reviews.
- Version maintainer evidence schemas separately from the public agent v2 contract.
- Reject JSON numeric overflow, relabeled crash reports, symlinked license resources and corrupt archive streams.
- Preserve Windows static CRT build flags and ship all IR schemas and referenced offline guides.
- Document installation, real beta consent, before-fix regressions and release review procedures.

- Add a pinned DS9 benchmark runner and locked dependency enforcement.
- Add deterministic release packaging, target validation and SHA-256 verification.
- Add installation instructions and the workspace-declared dual license texts.

## 0.1.4

Existing source version at the start of DS9-DS12 continuation. This entry does not
claim that a binary release or stable V1 has been published. The Git history is
the detailed record of earlier engine and CLI changes.
