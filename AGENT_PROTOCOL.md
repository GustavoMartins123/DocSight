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

Agent errors use `schemas/v2/error-envelope.json` and include a stable diagnostic code and process exit code. Exit code `0` is success; non-zero codes must be handled programmatically without matching human messages.

The M12 result contracts are `schemas/v2/spatial-query-result.json`, `schemas/v2/overview-result.json`, `schemas/v2/semantic-viewport.json`, and the shared `schemas/v2/semantic-object.json`.

The M13 diff contract is `schemas/v2/diff-result.json`; its NDJSON event type is defined in `schemas/v2/ndjson-event.json`.
