# Public-document compatibility validation

Latest syntax correction: [identical dictionary normalization](PDF_DICTIONARY_NORMALIZATION.md).
The original book now passes the identical preference duplicate and exposes a
separate conflicting transparency group; this is not a whole-document success.

Latest PDF correction: [table-inference follow-up](PDF_ALIGNMENT_V2.md), with
unchanged source inputs and separately recorded observations.

This file preserves the original baseline. See [the normalized-profile
follow-up](NORMALIZED_PROFILE.md) for the subsequent fixes and repeat campaign;
the baseline failures below are not the current results.

## Verdict

**Not ready for general-purpose document inspection.** On 2026-10-20, none of
seven independently published documents completed the tested reading and
rendering workflow. A successful command or a valid error envelope is not a
successful document review.

This is a small compatibility investigation, not a statistically representative
benchmark, security certification, independent-user beta, or release approval.
No engine code, acceptance threshold, golden, or expected failure was changed to
make these observations pass.

## Inputs and reproduction identity

- Source revision: `a9b6de2131ae0592c3f777b0d8e2d7ffca7910a6`.
- Engine: `0.1.4`, Linux x86_64, optimized build of the checked-out source.
- Executable SHA-256:
  `ca11c51750745141d884831004be44af7edf50dad5fc7eb6cfc11ee330f8e32d`.
- Build: `cargo build --locked --offline --release -p docsight-cli`.
- Seven downloads, 10,652,844 bytes in total. No document was generated from
  project specifications, implementation plans, or milestones.
- [sources.json](sources.json) records original publication pages, direct
  downloads, rights notes, sizes, and exact input SHA-256 values.
- [observations.json](observations.json) records measured outcomes, durations,
  output digests, diagnostics, supplementary tests, and limitations.

The downloaded documents, raw stdout/stderr, local orchestration scripts, exact
Arial faces, and oracle images are outside Git in
`/tmp/docsight-public-corpus`. That directory is temporary; retain it separately
if raw evidence is needed after a reboot. The scripts are development-only
orchestration, not an additional product execution path or test dependency.
The installed `~/.local/bin/docsight` was not replaced; these observations use
`target/release/docsight` from the revision above.

The downloads came from publisher or government publication pages, not document
mirrors of unknown origin. Think Python and Think Stats are openly distributed
Creative Commons books; their noncommercial conditions still apply. The forall x
publisher declares CC BY 4.0. GOV.UK publications carry Open Government Licence
notices, subject to their exceptions. NIST provides the public templates.
No downloaded document or extracted proprietary font is redistributed here.

## Observed compatibility

| Input | Publication | Observation |
| --- | --- | --- |
| Think Python, second edition | [Green Tea Press](https://greenteapress.com/wp/think-python-2e/), PDF, 244 pages | Inspection, navigation, extraction, search, and coverage returned JSON. Rendering pages 1 and 122 failed with `LAYOUT_PARTIAL`: glyph outlines were unavailable for `URWPalladioL-Roma` / `URWPalladioL-Bold`. Structure contains a confirmed false-positive table on the cover. |
| Think Stats, second edition | [Green Tea Press](https://greenteapress.com/wp/think-stats-2e/), PDF, 264 pages | Inspection, navigation, extraction, and search hit `RESOURCE_LIMIT` for Form XObject nesting, maximum 12. Rendering page 1 separately failed with `LAYOUT_PARTIAL` for missing glyph outlines. |
| forall x: Calgary, Fall 2025 | [Open Logic Project](https://forallx.openlogicproject.org/), PDF, 437 pages | All tested operations failed with `MALFORMED_DOCUMENT`: duplicate PDF dictionary key. The error does not identify the offending dictionary. Whether this is a parser defect or strict rejection of duplicate source entries remains unestablished. |
| CUI System Security Plan Template | [NIST](https://csrc.nist.gov/pubs/sp/800/171/r2/upd1/final), DOCX | All tested operations failed with `MALFORMED_DOCUMENT`: inline runs disagree with a list item's text. The reported object is `li_3d67c4aa4273f2cb7afc0af2ffdaf6f5`. |
| CUI Plan of Action Template | [NIST](https://csrc.nist.gov/pubs/sp/800/171/r2/upd1/final), DOCX | All tested operations failed with `LAYOUT_PARTIAL`: unsupported `sdt` content in `/word/footer2.xml`. |
| Model Services Contract front sheet, v2.2 | [GOV.UK](https://www.gov.uk/government/publications/the-model-services-contract-front-sheet-england-wales), DOCX | All tested operations failed with `LAYOUT_PARTIAL`: unsupported `sdt` content in `/word/footer1.xml`. |
| Model Services Contract core terms, v2.2A | [GOV.UK](https://www.gov.uk/government/publications/the-model-services-contract-core-terms-england-wales), DOCX | Initially blocked by missing exact Arial. After supplying all four exact Arial faces, inspection failed with `MALFORMED_DOCUMENT`: paragraph text and inline runs disagree, object `p_3b1da899ae50f4d306a8ccc823c70269`. |

PDF page counts in this table were independently observed with the development
oracle. They are not inferred from the failed DocSight outputs. No independent
DOCX pagination oracle was used.

## Campaign and protocol checks

Each input was exercised through `--agent --sandbox` with `inspect` twice,
`overview`, `text`, `find`, `tables`, `coverage`, and page-1 rendering at 72 DPI.
An additional middle-page render was attempted when inspection exposed a page
count. The default security/resource limits were not relaxed.

The 57 baseline invocations produced:

- 7 exit-zero results, all from Think Python, including its repeated inspection.
- 50 typed failures: 19 exit `20`, 16 exit `11`, 8 exit `30`, and 7 exit `13`.
- 8 render attempts and **zero generated DocSight PNGs**.
- Valid JSON envelopes and clean stdout/stderr separation for all 57 invocations.
- Byte-identical repeated inspection stdout, stderr, and exit status for all
  seven inputs, including failed inspections.

There were two supplementary CLI invocations, not included in that baseline:
full Think Python text extraction and the exact-Arial contract retest. The full
text command returned 1,471 blocks without truncation, but included
`PDF_TEXT_CODE_UNMAPPED`, `PDF_FONT_OUTLINE_MISSING`, and nine Unicode replacement
characters. This is not evidence of complete or accurate extraction.

A policy-restricted MCP server was also exercised with `inspect_document` and
`search_document` on every input: 14 tool calls, 2 with `isError: false` and 12
with `isError: true`. The server remained responsive to `ping` after the failed
calls. All tool responses retained the `untrusted_document` boundary. All 50
baseline CLI error envelopes, 14 MCP tool-result envelopes, and 12 MCP error
payloads passed their existing JSON Schema checks. Protocol correctness does not
remove the document-compatibility failures.

## Independent PDF oracle

The already installed MuPDF Python package was invoked explicitly as a
development-only external oracle. It was not added to the workspace, product,
normal test runner, CLI, or MCP and was never substituted for a failed DocSight
operation.

The oracle ran in a separate Bubblewrap sandbox with network isolation,
read-only document inputs, a private output directory, a 60-second CPU limit,
and a 1.5 GiB address-space limit. It opened all three books with
`is_repaired: false`, extracted text, and rendered first, middle, and last page
samples for each book. No MuPDF warnings were recorded. This confirms practical
interoperability with another reader, not formal PDF conformance or correctness
of every page.

Visual inspection of the oracle's Think Python cover confirms that its title
and subtitle are not a table. DocSight nevertheless reports that region as an
eight-column, two-row alignment table, confidence `0.75`, with `review: false`.
The global count of 847 inferred tables therefore must not be treated as a
validated table count; only the cover false positive was individually checked.

DocSight's Think Python `inspect` also advertises `render.available: true`, while
the tested pages cannot be rasterized. Coverage reports unsupported visual
evidence and zero global geometry score. Availability, fidelity, and actual
operation success need to be reconciled; an agent must not interpret the
capability flag as a render guarantee.

## Exact-font environmental retest

Arial was obtained from the original Microsoft Core Fonts package distributed
by the SourceForge Core Fonts project. The installer was not executed. Its TTF
files were extracted to a private directory and selected with
`DOCSIGHT_FONT_DIR`; no system font was installed and no substitution was used.
The package and all four faces are digest-bound in `observations.json`.

Resolving the original `FONT_UNAVAILABLE` error exposed the paragraph/run
disagreement in the core-terms DOCX. Supplying fonts therefore does not by itself
make that document usable. The baseline failure and supplementary failure are
both preserved rather than replacing one with the other.

## Reproduction commands

Download the original URLs in `sources.json` into the recorded relative
`documents/` paths beneath a chosen local `CORPUS` directory. Verify their exact
SHA-256 values before comparing observations; publisher download URLs may change
content. Downloads are an explicit preparation step, never a runtime requirement.

From the repository root, representative original failing invocations are:

```sh
cargo build --locked --offline --release -p docsight-cli
CORPUS=/tmp/docsight-public-corpus
target/release/docsight --agent --sandbox inspect "$CORPUS/documents/think-stats-2.pdf"
target/release/docsight --agent --sandbox render "$CORPUS/documents/think-python-2.pdf" --page 1 --dpi 72 --out "$CORPUS/think-python-page-1.png"
target/release/docsight --agent --sandbox inspect "$CORPUS/documents/forallx-calgary.pdf"
target/release/docsight --agent --sandbox inspect "$CORPUS/documents/nist-security-template.docx"
target/release/docsight --agent --sandbox inspect "$CORPUS/documents/nist-action-template.docx"
target/release/docsight --agent --sandbox inspect "$CORPUS/documents/model-contract-front.docx"
DOCSIGHT_FONT_DIR="$CORPUS/fonts/arial" target/release/docsight --agent --sandbox inspect "$CORPUS/documents/model-contract-core.docx"
```

Read and retain each exit status and stderr. These commands currently fail; they
are not examples of passing acceptance checks. Do not remove isolation, bypass a
policy denial, raise limits, repair/rewrite the inputs, substitute fonts, or switch
renderers to label these same cases successful.

## Remediation priorities

1. Minimize and fix the OOXML text/run disagreement without discarding content
   or removing the consistency invariant. Regress both the NIST template and the
   font-resolved contract against the original digest-bound files.
2. Implement canonical interpretation of supported static `sdt` content in
   headers/footers, with explicit diagnostics for unimplemented binding or
   formatting semantics; never fetch external data.
3. Investigate native PDF font decoding/rasterization and text mapping for these
   books. Exact embedded programs must remain the source of glyph evidence.
4. Identify the exact source of the Form nesting rejection and duplicate-key
   rejection. Do not assume that increasing limits or accepting malformed input
   is a valid fix.
5. Correct table false positives and review/confidence calibration, and align
   capability reporting with known rendering limitations.

Every correction needs a small generic regression, the appropriate engine and
contract tests, and a repeat of the unchanged external inputs. Passing this
seven-document campaign would be necessary progress, not proof that the broader
release-readiness requirements have been met.

No proof bundles, cross-document diffs, Windows/macOS runs, complete pixel
comparisons, or reviewed text-accuracy scores were established in this campaign.
