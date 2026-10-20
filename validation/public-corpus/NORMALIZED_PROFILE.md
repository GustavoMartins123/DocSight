# Self-contained fonts: public-corpus follow-up

## Result and limits

Six of seven unchanged published documents completed the tested CLI workflow:
inspection, overview, bounded text, search, table listing, page-1 coverage, and
first/middle-page rendering. This establishes compatibility for these operations,
not complete extraction, accurate table detection, pixel parity, or release
readiness. The [original baseline](README.md) remains preserved.

The DOCX view is explicitly normalized, not a reproduction of Word pagination.
No source font downloads or installations are required. The profile selects
bundled fonts before layout, irrespective of which fonts the host has installed;
it never attempts exact fonts and then retries with substitutes. Original font
names remain in semantic IR. Unsupported characters still produce an explicit
error, not an arbitrary glyph or another font.

PDF visual evidence continues to use embedded source programs. No PDF page was
converted through the DOCX font policy or an external renderer.

## Reproduction identity

- Observation date: 2026-10-20; Linux x86_64.
- Base revision: `a9b6de2131ae0592c3f777b0d8e2d7ffca7910a6`, **with uncommitted
  implementation changes**. These results must not be attributed to clean HEAD.
- Engine version: `0.1.4`; IR contract: `1.5`.
- DOCX layout profile: `docx-normalized-v1`; PDF profile: `pdf-native-v2`.
- Build: `cargo build --locked --offline --release -p docsight-cli`.
- Tested executable: `target/release/docsight`; SHA-256:
  `c7816992b3bd489e73047e163848e0ac8c09b781cb198fc18cfd1d430067c092`.
- The globally installed CLI was not replaced by this change.
- Input URLs, rights notes and exact digests remain in [sources.json](sources.json).
- [normalized-observations.json](normalized-observations.json) records measured
  calls, output digests, diagnostics and artifact verification without copying
  book text, images, input binaries or extracted fonts into the repository.

Raw command outputs, development-only orchestration and generated artifacts are
under `/tmp/docsight-public-corpus`. This temporary directory must be retained
separately if raw evidence is needed after cleanup or reboot. Downloads were an
explicit corpus-preparation step, never a product runtime dependency. Measurements
were taken during development and are not isolated performance benchmarks.

## Unchanged inputs, retested

| Input | Observed pages | Tested workflow |
| --- | ---: | --- |
| Think Python, second edition, PDF | 244 | Completed, including first/middle-page PNGs. Unmapped text codes remain diagnosed; a previously confirmed cover-table false positive remains unresolved. |
| Think Stats, second edition, PDF | 264 | Completed, including first/middle-page PNGs. Unmapped codes and missing outlines elsewhere remain diagnosed; this is not all-page render coverage. |
| forall x: Calgary, Fall 2025, PDF | Not returned | Rejected with `MALFORMED_DOCUMENT`, exit 11; no partial render artifact. |
| NIST CUI System Security Plan Template, DOCX | 22 normalized | Completed; normalization and computed-pagination diagnostics retained. |
| NIST CUI Plan of Action Template, DOCX | 2 normalized | Completed; normalization and computed-pagination diagnostics retained. |
| GOV.UK Model Services Contract front sheet, DOCX | 22 normalized | Completed; normalization and computed-pagination diagnostics retained. |
| GOV.UK Model Services Contract core terms, DOCX | 100 normalized | Completed; unsupported body elements and unresolved numbering remain diagnosed in addition to normalized pagination. |

The rejected PDF contains two `/ViewerPreferences` entries in the same source
dictionary, beginning at zero-based file bytes 1471629 and 1471733. Both declare
`DisplayDocTitle true`. These were checked in the original bytes. The diagnostic
identifies `/ViewerPreferences` and the parser-buffer cursor; that cursor is not
a promise of an absolute file offset. Strict dictionary uniqueness was not
relaxed, and the source file was not repaired or rewritten. Another reader's
ability to open it does not establish formal conformance.

## Changes exercised

- Bundled DejaVu Sans and Liberation Serif/Mono provide the canonical DOCX
  visual profile, including styled faces and content fingerprints. No installed
  system fonts or external font directories influence this path.
- DOCX `text` uses semantic ingestion independently of pagination and shaping.
  A separate synthetic regression verifies extraction of unrenderable text while
  visual rendering fails without publishing an artifact.
- Paragraph property tab stops no longer introduce spurious text characters.
  Static header/footer content controls are interpreted; data-bound controls are
  rejected rather than fetching resources.
- Embedded Type 1 and supported raw CFF programs are decoded through a pinned,
  licensed font-only adapter and painted by DocSight's native rasterizer, with
  bounded outline generation. Unsupported embedded formats fail explicitly.
- Only invoked Form XObjects are expanded; the existing recursion limit stays
  unchanged. Deterministic PDF text-block identities distinguish repeated labels.
- Empty table-cell paragraph whitespace is anchored once instead of generating
  invalid zero-offset continuations on later pages.

## Campaign checks

The CLI campaign used `--agent --sandbox`, existing default resource limits and
the same input digests as the baseline. It comprised 62 invocations: 54 exit-zero
results and eight exit-11 failures, all for the duplicate-key PDF. Twelve of
13 render attempts produced PNGs; only first and middle pages were sampled.
Repeated inspections of all seven inputs were byte-identical, including errors.
All commands retained valid JSON and clean stdout/stderr separation.

A restricted MCP policy exercised inspection and search for every document:
14 tool calls, 12 successful results and two typed errors. A subsequent `ping`
succeeded. Ten further successful calls exercised search → context → object crop
→ proof bundle → replay for one NIST DOCX and Think Python PDF. Both bundles
passed document, crop and trace verification. Repeated crops and bundles were
byte-identical across different output directories. Every tool result retained
the `untrusted_document` boundary.

Agent success/error envelopes, typed CLI results, MCP tool envelopes and MCP
error payloads were checked against their existing JSON Schemas. This validates
protocol shape, not factual accuracy of the returned evidence.

The implementation was also checked with `cargo fmt --all -- --check`,
`cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`
and `cargo test --locked --offline --workspace --all-features`. The workspace
suite includes synthetic regressions, deterministic IR/render snapshots, agent
contracts, worker isolation and third-party license-notice checks. These are
complementary to, not substitutes for, the external-document campaign.

## What remains unvalidated or unresolved

- No reviewed full-text accuracy score, complete per-page rendering campaign,
  DOCX-versus-Word comparison or comprehensive pixel-difference score exists.
- PDF table inference still has a confirmed false positive and needs detector
  calibration; returned table counts are not validated ground truth.
- Unmapped PDF codes, unembedded font outlines, unsupported OOXML body content
  and numbering diagnostics must remain visible to consumers.
- Bundled fonts do not cover every script, symbol or arbitrary private-use
  character. Unsupported glyphs do not authorize a missing-font retry.
- These execution checks were Linux-only; Windows and macOS were not run.
- No parser permissiveness, resource-limit increase, alternate renderer or
  expected-failure conversion was used to label the seventh document successful.

The next useful quality increment is a separately reviewed extraction and table
accuracy corpus, rather than presenting this compatibility result as universal
document fidelity.
