# Agent-first protocol

Use `--agent` when a caller needs the canonical machine contract. The profile emits JSON on stdout, keeps diagnostics out of stderr on successful requests, and emits versioned structured errors on stderr.

Discover the current command surface before selecting an operation:

```text
docsight --agent capabilities
```

Inspect a document before requesting detailed objects:

```text
docsight --agent --sandbox inspect input.docx
```

Use NDJSON for large collections and persist the continuation token from the `done` record:

```text
docsight --agent --ndjson text input.docx --max-items 100 --text-limit 2000
```

Navigate a document before materializing its text or tables:

```text
docsight --agent overview input.pdf --max-items 40
docsight --agent focus input.pdf tbl_0008 --related --max-items 12
docsight --agent focus input.pdf --pages 20..22 --max-items 40
```

`overview` returns headings, tables, and figures in canonical page/reading order. `focus` returns the target or page range plus nearby reading-order objects and explicitly-labelled relationship provenance. `--related` adds deterministic caption and source-anchor note relations for an object target. Semantic-object text is a 240-character snippet; `text_truncated` tells the caller when the source text is longer. Use `table`, `text`, or `page` when complete content is required. Machine results default to 64 viewport objects and 100 query matches; continuation tokens retrieve the remainder. A focus page range is capped at 32 existing pages.

Spatial DQL uses only canonical page-local points. It never infers coordinates from pixels:

```text
docsight --agent query input.docx 'table below(heading contains("Revenue"))'
docsight --agent query input.pdf 'figure nearest(caption)'
docsight --agent query input.docx 'paragraph overlaps(watermark)'
docsight --agent query input.pdf 'object distance-to(tbl_0008) < 24pt'
```

Supported relation names are `above`, `below`, `inside`, `overlaps`, `nearest`, and `distance-to`. Relative relations apply only to objects on the same page. `above` and `below` additionally require horizontal overlap. `distance-to` accepts only a positive value in `pt`; other units fail with a typed usage error. Results identify the canonical first anchor, `matching_anchor_count`, relation, and edge-to-edge distance in points; a count above one exposes geometric ambiguity. Objects without canonical geometry are counted and reported with `SPATIAL_GEOMETRY_UNAVAILABLE` rather than silently treated as spatial matches. `caption` currently means a DOCX paragraph whose style is `Caption`; the selector fails closed for PDF because caption semantics are not yet reconstructed there.

Viewport visual references are deterministic crop requests at 36 DPI. They carry page-local geometry and do not write or embed an image artifact; `artifact_available` is therefore always `false`. Use the existing `crop` command with the emitted page and bounding box when pixels are necessary.

Use `peek` for compact structural sight without materializing full object content:

```text
docsight --agent peek input.docx --page 7
docsight --agent peek input.pdf --pages 20..22
docsight --agent peek input.docx --object tbl_0008 --related
docsight --agent peek input.docx --section 1
```

Exactly one target is required. Page ranges are inclusive and limited to eight existing pages. Section targeting uses the canonical one-based DOCX section index and fails closed when the current layout cannot map a multi-section document to canonical page ranges. A peek result contains 240-character semantic snippets and typed relationship hints; it never embeds raster artifacts or full table content.

Use `context` to aggregate evidence for an explicit object or to perform deterministic lookup and expansion in one call:

```text
docsight --agent context input.docx tbl_0008 --include content,neighbors,geometry,fidelity,provenance,heading,related
docsight --agent context input.docx --find "quarterly revenue" --kind table
```

`content` is the canonical typed DIR content, including table spans and nested cell blocks. `neighbors`, `heading`, and `related` carry explicit roles, confidence, and provenance. `geometry`, `fidelity`, and `provenance` remain separate evidence surfaces. A `--find` request returns its descriptor, matched character range, selected object, and ranking components. Ambiguous or weak matches return `status: "ambiguous"` with ranked candidates and no silently chosen context. `--kind` is valid only with `--find`.

Use `resolve` when the caller needs ranked navigation candidates rather than an expanded context:

```text
docsight --agent resolve input.docx --text "revenue table" --kind table
docsight --agent resolve input.pdf --text "director signature" --pages 10..14
```

Resolve uses normalized lexical evidence, token overlap, object-kind and page constraints, nearby caption text, nearest preceding heading text, and canonical geometry for caption proximity. It does not use embeddings, a remote model, or natural-language question answering. Each candidate exposes the component score, weight, contribution, evidence anchor, and direct matched range when available. Scores below the resolution threshold and ties within the fixed ambiguity margin remain explicitly ambiguous.

Use `--budget` when the JSON envelope should adapt its evidence projection before serialization:

```text
docsight --agent context input.docx tbl_0008 --budget 12kb
docsight --agent peek input.pdf --pages 20..22 --budget 4kb
docsight --agent context input.docx tbl_0008 --budget-profile compact
```

Budgets are deterministic serialized-byte limits. Bare integers and the `b`, `kb`, and `mb` suffixes are accepted; `kb` and `mb` use powers of 1024. Without a fixed profile, DOCSIGHT tests `rich`, `balanced`, then `compact` against the complete JSON envelope and chooses the first representation that fits. Collection results may then expose a deterministic continuation boundary if even the compact form cannot carry every item. No result is emitted when one compact item and the envelope cannot fit.

`rich` preserves every requested evidence class. `balanced` keeps structural context, snippets, geometry, fidelity, provenance, neighbors and ranking evidence while omitting full object content, related objects and viewport visual references when present. `compact` preserves identities, kinds, pages, containing structures and headings while additionally omitting extended snippets, semantic neighbors, geometry, fidelity, provenance, matched ranges and ranking components when present. `limits.projection` records the requested byte budget or fixed profile, the selected profile, whether selection was adaptive and every evidence class actually omitted. Warnings are not removed to satisfy `--budget`.

`--budget-profile compact|balanced|rich` fixes the policy instead of allowing a lower tier. Collections may still expose a continuation boundary; a single result or one collection item that does not fit a simultaneous `--budget` fails with a typed usage error. `--max-bytes` remains an independent hard safety cap. Adaptive budgets require a complete bounded JSON envelope; NDJSON streams continue to use `--max-bytes` and reject `--budget` and `--budget-profile` explicitly.

Compare revisions through the same machine profile:

```text
docsight --agent diff before.docx after.docx
docsight --agent --ndjson diff before.pdf after.pdf --max-items 100
```

The diff result identifies both source documents in `before_document` and `after_document`. `semantic.lineage` is the cross-version correspondence layer; it does not replace either document-bound object ID. A `matched` record has one object from each revision, a deterministic `lin_…` identifier, a match score, and the evidence components used to establish correspondence. Components can include normalized text, source path, style, geometry, table shape, image digest, neighborhood, and reconstruction confidence.

An `ambiguous` lineage record intentionally omits a definitive counterpart and returns the competing candidates with their own evidence and scores. Related semantic additions or removals carry that lineage record and set `authoritative` to `false`. A changed record is likewise non-authoritative when relevant diagnostics or low-confidence reconstruction evidence are present. Inspect `evidence` before treating a diff as proof of a source-level content change. NDJSON emits `diff.summary` with both document identities, then `diff.lineage`, then `diff.semantic` records.

Every document result carries a deterministic document ID and SHA-256 digest. Use `warnings`, `capability_details`, and `coverage` before treating geometry or pixels as authoritative. `source_faithful` distinguishes an available operation from an exact representation.

Output limits are explicit. `limits.truncated` describes omitted result items, while `limits.warnings_truncated` describes omitted diagnostics. A continuation token is valid only for the command, document, and query or focus target that produced it.

Render and crop results include the requested output path, PNG media type, byte count, SHA-256 digest, page bounding box, and pixel dimensions. The digest verifies the artifact; fidelity still comes from the warnings and coverage records.

Record deterministic evidence for a rendered page, then replay it without the original document path:

```text
docsight --agent render input.docx --page 7 --trace page7.dstrace --out page7.png
docsight --agent replay page7.dstrace --verify
```

A `.dstrace` is a canonical, self-contained ZIP artifact with `manifest.json` and `source.bin`. Its manifest records the source digest, reproduction fingerprint, page-local target, resolved resources, glyph metrics, layout decisions, the normalized display list and the raster fingerprint. PDF display lists include ordered text, fill, stroke and figure operations with paths, clipping regions, alpha and stroke styles. `replay` requires `--verify`, re-executes from the embedded bytes, and fails closed with `VERIFICATION_FAILED` if any claimed result differs. Each `decision_coverage` field is `verified`, `not_applicable` or `unavailable`; only a true `unavailable` gap produces `TRACE_DECISION_PARTIAL`. PDF line breaking, table sizing and pagination are `not_applicable` because those decisions are encoded by the source page content rather than made by DOCSIGHT.

Use a proof bundle when an agent needs one object or one region with the selected semantic evidence, provenance, geometry and optional crop in one offline-verifiable package:

```text
docsight --agent bundle input.docx --object tbl_0008 --include-crop --out tbl_0008.dse
docsight --agent verify tbl_0008.dse
docsight --agent bundle input.pdf --page 3 --bbox 72,144,324,288 --out region.dse
```

A `.dse` has fixed ordered entries: `manifest.json`, `source.bin`, and, when requested, `crop.png`. It is content-addressed by the returned SHA-256, contains no original filesystem path, and is accepted only in its canonical stored form. `verify` replays the embedded trace and recomputes evidence from the embedded source. Unknown entries, compression, excess sizes, checksum disagreement and non-canonical archives fail closed. Agent responses for `render --trace` and `replay --verify` expose the trace schema, target, decision coverage and resource, glyph-run, table-sizing, pagination and display-operation counts directly, so consumers can assess completeness without opening the artifact. The public contracts are `schemas/v2/trace-manifest.json`, `schemas/v2/proof-bundle-manifest.json`, `schemas/v2/trace-result.json`, `schemas/v2/bundle-result.json`, `schemas/v2/replay-result.json`, and `schemas/v2/verify-result.json`.

Agent errors use `schemas/v2/error-envelope.json` and include a stable diagnostic code and process exit code. Exit code `0` is success; non-zero codes must be handled programmatically without matching human messages.

The M12 result contracts are `schemas/v2/spatial-query-result.json`, `schemas/v2/overview-result.json`, `schemas/v2/semantic-viewport.json`, and the shared `schemas/v2/semantic-object.json`.

The M13 diff contract is `schemas/v2/diff-result.json`; its NDJSON event type is defined in `schemas/v2/ndjson-event.json`.

The M16 interaction contracts are `schemas/v2/peek-result.json`, `schemas/v2/context-result.json`, and `schemas/v2/resolve-result.json`.

The M17 adaptive projection contract is `schemas/v2/projection-selection.json`, referenced by `schemas/v2/agent-envelope.json`.
