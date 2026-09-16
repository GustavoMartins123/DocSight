# Backlog

Work that is known, deliberately deferred, and classified by whether it blocks the first serious release.

`PRODUCT_SCOPE.md` states what DocSight promises today. This file states what it does not promise yet and when that is expected to change. Nothing here is a commitment to a date; the classification is about ordering, not scheduling.

Classification:

- **v1** — required before DocSight can be installed and trusted by a third party. A gap that makes a common real document unusable, or that makes an existing promise unreliable.
- **post-v1** — real value, but the product is honest and useful without it.
- **experimental** — unproven direction. Needs a use case, a contract and a test strategy before any code.

---

## v1

### Ingestion

| Item | Why it blocks v1 |
| --- | --- |
| Incremental PDF updates (`Prev` chains from appended revisions) | Common in signed and annotated PDFs. Tracked as A1b. |

### Layout fidelity

| Item | Why it blocks v1 |
| --- | --- |
| Multi-section DOCX geometry | Section geometry after the first section is discarded (`DOCX_SECTIONS_COLLAPSED`), so page size, margins and headers are wrong for any document that changes section. |
| Line-level pagination | Pagination moves whole blocks, so widows, orphans and `keep-lines` are approximations and page breaks can occur earlier than in Word. |

### Operability

| Item | Why it blocks v1 |
| --- | --- |
| A command-line way to supply a PDF password | The standard security handler is implemented and reachable from the library (`PdfDocument::open_with_password`, `docsight_ingest::ingest_with_password`), but no CLI surface exposes it, so a document with a real user password cannot be opened from the terminal. Deferred rather than rushed because it needs a route that does not leak the password into `argv` — a `--password-file` is the likely shape — and because it threads a new parameter through every command handler. |
| Content-addressed cache between invocations | An agent workflow re-parses and re-lays-out the same document for every command. Tracked as A13. |
| Per-invocation overrides for ingestion limits | The enforced values are centralized and published in `capabilities` under `ingestion_limits`, and each boundary is tested, but a caller cannot raise one for a document that legitimately exceeds it. Deferred until a real document forces the question, so the override surface is designed against an actual case rather than guessed. |

---

## post-v1

- Real font discovery, loading and shaping instead of the deterministic proportional fallback, with a declared font fingerprint in the reproducibility inputs.
- Filtered image scaling. Figure scaling is nearest-neighbour, chosen for determinism; a box or Lanczos filter would look closer to Word at the cost of a defined tolerance in the visual goldens.
- Remaining PNG variants: 1, 2, 4 and 16 bits per channel, and interlaced images. Each currently fails closed with a diagnostic naming the variant.
- Shading (`sh`) painting. The operator no longer vetoes a document, but the shaded area is left unpainted and reported through `PDF_SHADING_UNSUPPORTED`.
- Image XObject decoding. PDF image pixels are still placeholders; only DOCX PNG parts are decoded today.
- Contextual spacing (`w:contextualSpacing`), which is parsed and reported but not applied between paragraphs of the same style.
- Annotation appearance streams: an annotation currently contributes geometry and text to the IR, not its rendered pixels.
- `Watermark` overlays in the IR. The entity exists but no parser produces it.
- DOCX floating objects and shapes, and the `Shape` block kind, which is likewise declared but never produced.
- Per-revision tracked changes: authorship, timestamp and content, rather than insertion and deletion counts.
- Complex DOCX numbering: multi-level list restarts, custom patterns and style-linked numbering.
- Table columns, column spans and text flow across columns.
- PDF caption reconstruction, which would let the `caption` DQL selector work for PDF instead of failing closed.
- Table detection quality: calibrated confidence for unruled and partially-ruled tables, and a second detector with declared provenance.
- Richer `diff` lineage across versions, reducing `DIFF_LINEAGE_AMBIGUOUS`.
- Packaged distribution: signed release artifacts, install instructions and a supported-platform matrix.

---

## experimental

Nothing here is planned. Each item requires a written use case, a stable contract and a test strategy before implementation, per Rule 1 of the implementation plan.

- DQL beyond the current constrained selector grammar: joins, aggregation, user-defined predicates.
- A plugin interface for third-party detectors or exporters.
- Incremental or streaming ingestion of very large documents.
- A local viewer or TUI over the IR.
- Structured export targets beyond the current ones, such as a normalized archival format.

Items explicitly rejected rather than deferred are listed under *Out of scope* in `PRODUCT_SCOPE.md`.
