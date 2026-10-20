# PDF dictionary normalization

## Correction

The parser no longer rejects a dictionary merely because a name occurs twice.
Identical parsed values are normalized to one value with the explicit diagnostic
`PDF_DICTIONARY_DUPLICATE_NORMALIZED`. Its source evidence identifies the
dictionary, both key positions, containing indirect object when present, and
whether offsets belong to the PDF-header-relative buffer or a decoded object
stream. Source bytes are not rewritten.

Conflicting values still produce `MALFORMED_DOCUMENT` with a typed object and
offset. Different indirect references are not presumed equivalent. Numeric
types are not coerced; values containing real numbers additionally require
identical source spellings, avoiding accidental equality from float rounding.
No parser retry or first/last-key-wins route exists. The bounded normalization
budget is 1024 distinct source occurrences; rereading an occurrence does not
consume the budget twice. Public record shapes remain unchanged, and the
reproduction profile includes `dictionary-normalization-v1`.

## The original book: two distinct findings

The unchanged `forallx-calgary.pdf` has SHA-256
`9eca9e08ad6380b00a783536f8a16abc94d13d9561f05baa7fc0a2b762e48ef0`.

1. The duplicate `/ViewerPreferences` entries both contain
   `/DisplayDocTitle true`. This duplication is now accepted and diagnosed.
2. Reading further exposes a **different, genuinely conflicting duplicate** in
   object `9 0 R`, a Form XObject. At zero-based source byte 10566 it declares
   `/Group << /S /Transparency /K false /I false >>`. At byte 10765 it declares
   `/Group 14 0 R`. Object `14 0 R` declares `/Type /Group`, `/S /Transparency`,
   `/I true` and `/CS /DeviceRGB`.

The original file therefore now fails at `/Group`, not `/ViewerPreferences`.
The isolation values differ. Selecting one group would silently choose between
different compositing meanings; equal-value normalization cannot resolve this.
This is evidence of an ambiguous source object, not evidence that the book is
malicious. Earlier reports identified only the first parse failure and did not
establish the absence of additional conflicts.

The two values and offsets above were checked directly in bounded original
bytes. No repaired copy was substituted, and the remaining failure is not
reported as a successful whole-document workflow.

## Validation

Generic fixtures exercise identical catalog/trailer entries, dictionary order
and whitespace, decoded names and strings, nested values, references, exact
real numbers, conflicting scalars/arrays/dictionaries, float-rounding collisions,
decoded object-stream locations and normalization limits below/at/above the
boundary. Indirect metadata conflicts remain errors rather than optional
metadata warnings.

Agent CLI regressions exercise inspection, text extraction and PNG rendering
under isolation. Identical duplicates produce successful results with the
normalization diagnostic; repeated isolated and direct inspections are
byte-identical. Conflicts produce typed errors without publishing an artifact.
MCP inspection now preserves diagnostics through its existing additional-content
records instead of discarding them; a dedicated regression compares these
records with CLI warnings. The primary inspection result shape is unchanged.

The real-document CLI/MCP repeat campaign retains the original inputs and uses
`target/release/docsight`, not the globally installed executable. Machine
observations, executable identity, error locations and test outcomes are in
[pdf-dictionary-observations.json](pdf-dictionary-observations.json). Raw outputs
remain under `/tmp/docsight-public-corpus` and
`/tmp/docsight-duplicate-validation`; retain those temporary directories
separately if raw evidence must survive cleanup. Earlier reports are preserved.
