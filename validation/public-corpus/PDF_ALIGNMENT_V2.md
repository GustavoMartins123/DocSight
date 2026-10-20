# PDF reading: table-inference correction

## Scope

This follow-up corrects PDF semantic reconstruction, not its visual appearance.
The previous detector could mistake title/subtitle words and ordinary prose for
columns, replacing readable text with a spurious grid. The goal is useful
reading, navigation and table extraction, not emulating a desktop reader.

The canonical unruled detector is now `alignment-v2`, with reconstruction
fingerprint `pdf-native-v3/alignment-v2`. It groups nearby words into cells,
requires recurring left or right edges and whitespace gutters, checks candidate
density, and distinguishes tightly spaced prose columns. Header rows require
formatting or column-type evidence. Short textual grids without stronger
evidence remain exposed with confidence below the existing review threshold.
Scores are heuristic evidence summaries, not calibrated accuracy probabilities.

Runs outside actual cell geometry are not removed merely because they fall
inside a table's enclosing rectangle. Rejected candidates leave ordinary text
available to paragraph reconstruction. No alternate parser, render fallback,
network resource, OCR dependency or source-document rewrite was introduced.

Each inferred grid is bounded to 262144 positions before cell allocation.
Oversized candidates fail with a typed resource error, without partial grids.
The Rust detector now returns `Result`; all callers propagate or explicitly
test failures. The detector/profile versions distinguish changed inference and
provenance without retaining an implicit old execution route.

## Reproduction identity

- Date: 2026-10-20; Linux x86_64.
- Base revision: `cc941c6`, **with uncommitted implementation changes**.
- Engine `0.1.4`; IR record shape remains `1.5`.
- Build: `cargo build --locked --offline --release -p docsight-cli`.
- Executable: `target/release/docsight`, SHA-256
  `140dbf015c9fc07693699ce6be69c1f487c5f38bd634830f6ba8d60c4be2fc59`.
- The globally installed executable was not replaced.
- Original source URLs, rights notes and digests: [sources.json](sources.json).
- Measured records and output hashes:
  [pdf-alignment-v2-observations.json](pdf-alignment-v2-observations.json).

Original PDFs, stdout/stderr, scripts and artifacts remain outside Git under
`/tmp/docsight-public-corpus`. That temporary directory needs separate retention
if raw evidence must survive cleanup. Earlier observations and reports were not
overwritten. No downloaded book text or image is redistributed in this report.

## Unchanged external inputs

The three PDF downloads retain their original digests. The two readable books
completed repeated inspection, overview, bounded text, search, table listing,
page-1 inspection and coverage, and first/middle-page rendering under
`--agent --sandbox`. The third still fails strict duplicate-key validation.

| Input | Previous inferred tables | Current inferred tables | Checked observation |
| --- | ---: | ---: | --- |
| Think Python, second edition | 805 | 59 | The cover is now two ordinary heading objects with intact title/subtitle text, not a table. |
| Think Stats, second edition | 616 | 92 | The cover has no inferred table. A real 7×3 table on PDF page 30 remains detected, including multiword labels and right-aligned totals. |
| forall x: Calgary | Not returned | Not returned | `MALFORMED_DOCUMENT`: duplicate `/ViewerPreferences` in the original dictionary; no input repair or parser permissiveness added. |

The page-30 table was checked against DocSight's native render. Example checks
included the header `value / label / Total`, a multiword label with total `9148`,
and another with total `1862`. The first and middle PNGs for both books remained
byte-identical to the previous renderer. This change therefore corrects the
semantic interpretation without changing those sampled visual artifacts.

**Lower table counts are not a precision or recall score.** The remaining
59/92 candidates were not all manually labeled. Rejecting a false candidate
does not establish that every genuine table was retained, or that all text is
extracted accurately. Full-text accuracy and all-page visual coverage remain
unmeasured; unmapped text codes and missing font outlines elsewhere remain
explicit diagnostics.

## Executed verification

- Six new generic regressions were run before the fix and all failed: title
  fragments, unrelated column starts, right-aligned values, invented headers,
  multiword cells, and newspaper-like prose columns. All now pass.
- Existing ruled/unruled fixtures, irregular spacing, glyph joining,
  straddling cell text, CSV export, object resolution and crop tests passed.
- New PDF ingestion regression preserves each expected title/cell string once,
  validates canonical IR, and checks repeated ingestion and raster stability.
- Grid-budget tests exercise just below, exactly at and above the cell limit,
  plus arithmetic overflow; the ingestion limit matches the enforced constant.
- `cargo test --locked --offline --workspace --all-features`: **814 passed**,
  zero failures; two subprocess fixtures are excluded from direct execution.
- `cargo fmt --all -- --check` and
  `cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings`
  passed. The inference change required no new dependency.
- The existing span-clustering fuzz target compiled with the synchronized fuzz
  lockfile and completed a bounded 5000-run nightly sanitizer smoke campaign
  without failure. This is not exhaustive fuzzing.
- The external PDF campaign produced 29 CLI calls: 20 successful, nine typed
  exit-11 errors for the unchanged malformed input. All channels and envelopes
  passed checks, and repeated inspections were byte-identical.
- Restricted MCP inspection/search across the seven existing inputs produced
  12 successes and two typed errors, followed by a successful `ping`. Two
  repeated search → context → crop → bundle → replay workflows added 20
  successful calls. All four bundle replays verified crops and traces; repeated
  artifacts were byte-identical. All results retained `untrusted_document`.
- CLI envelopes and typed results, MCP envelopes and MCP errors passed their
  existing JSON Schema checks. Only trace fingerprints changed in the existing
  evidence conformance golden; sampled raster hashes stayed unchanged.

These checks were Linux-only. Scanned PDFs still do not imply OCR text support,
and strict rejection of ambiguous/malformed input remains intentional. Broader
compatibility and a reviewed table-accuracy corpus remain separate work; this
report does not claim the ability to read every possible PDF.
