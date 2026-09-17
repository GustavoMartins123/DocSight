# Product scope

DocSight is a local, headless, deterministic tool for inspecting, structuring, rendering, comparing and extracting evidence from DOCX and PDF documents.

This document answers one question: **what does DocSight promise today?**

It is normative. A command may not claim more than this document lists, and any behaviour listed under *Partial* must expose its limitation through a typed diagnostic rather than silently degrading. The command surface is declared programmatically by `docsight --agent capabilities`; the two are kept in sync by `crates/docsight-cli/tests/product_scope.rs`.

Everything runs offline. Parsing, layout, rendering, diffing and verification never require the network, Word, Excel, LibreOffice, COM automation or a remote service.

---

## Supported

Behaviour in this section is implemented, tested and source-faithful unless a document itself triggers a diagnostic.

### Input and identity

- DOCX (OOXML wordprocessing) and PDF, detected by magic bytes and never by file extension.
- Content-addressed document identity: `sha256` of the exact bytes, and a `doc_…` identifier derived from it.
- Deterministic object identifiers derived from the document digest plus a normalized semantic source path. The same bytes produce the same identifiers on every platform, regardless of file name or location.
- Resource limits on document size, archive entries, compression ratio, XML depth, token size, path segments, pages, annotations and layout iterations, each reported as a typed `RESOURCE_LIMIT` error. The enforced values are published as a machine-readable catalogue in `capabilities` under `ingestion_limits`, so a caller can see the exact boundary before sending a document.

### Document IR

- A single normalized `Document` IR shared by both formats, versioned by `schema_version` and `engine_version` and published as a JSON Schema at `schemas/ir/v1/document-ir.json`.
- Modelled entities: document, metadata, styles, sections, pages, blocks, table cells, overlays, resources, hyperlinks, comments, tracked-change counts and diagnostics.
- Block kinds: paragraph, heading, list item, table, figure, note, unknown.
- Canonical ordering is a contract, not a convention: pages ascend, blocks ascend by page and reading order, page indexes agree with block placement and every object identifier is unique. Ingestion validates this and fails closed with `BACKEND_FAILURE` if a producer breaks it.
- Fidelity is derived from the IR's own diagnostics by a single catalogue in `docsight-core`, so `inspect`, `coverage` and `evidence` cannot disagree about what a document lost.
- Every visible block carries, when knowable: identifier, kind, page, bounding box, z-index, reading order, source provenance and confidence.

### DOCX

- Paragraphs, headings, list items, structural tables, figures, footnotes and endnotes.
- Table grid with row and column spans, nested tables, cell text and cell geometry.
- Style definitions with `basedOn` inheritance, including the default paragraph style and document defaults for `keepNext`, `keepLines`, `pageBreakBefore` and `widowControl`.
- Sections from every paragraph-level and final `sectPr`: page size and orientation, margins, header and footer distances, start type (`nextPage`, `continuous`, `evenPage`, `oddPage`, `nextColumn`), title page, page-number restart and format, and default, first-page and even-page headers and footers inherited from the previous section as Word does. Every DOCX page records its `section_index`.
- Hyperlinks, comments and tracked-change counts.
- Package metadata from `docProps/core.xml` and `docProps/app.xml`: title, author, subject and the authoring application.
- Deterministic pagination and geometry per section, honouring explicit page breaks, `pageBreakBefore`, `keep-with-next`, `keep-lines` and widow/orphan control, which applies unless a document default, style or paragraph disables `widowControl`. Paragraphs, headings, list items and notes split by line across pages; such a block keeps one identifier, its first page and bounding box, and page-local `continuations` whose `text_start_char` locates each later fragment in the block text.
- Headers and footers projected per page from the section in effect, with `PAGE`, `NUMPAGES` and `SECTIONPAGES` fields resolved per page, including decimal, roman and letter page-number formats. Other header and footer fields keep their cached result with a diagnostic.
- Paragraph formatting from the style cascade and direct properties: alignment (`w:jc`), spacing before and after and automatic line spacing (`w:spacing`), and left, right, first-line and hanging indentation (`w:ind`).
- Unknown body elements are preserved as opaque nodes with a diagnostic; they are never dropped silently.

### PDF

- A native parser with no third-party document engine at runtime: cross-reference tables, cross-reference streams, hybrid `XRefStm`, object streams and `Prev` chains.
- Stream filters, page tree, page resources and content streams.
- Text extraction with encoding and `ToUnicode` resolution, embedded TrueType outlines, `Identity-H` CID fonts and CID-to-GID mapping.
- Paragraph and heading reconstruction with per-object confidence.
- Table inference with an explicit detector name and confidence score.
- Form XObjects are traversed: their content stream is parsed with the invoking graphics state composed with the form `/Matrix`, and their text, geometry and nested forms reach the IR. Cycles and nesting beyond 12 levels fail closed.
- Encrypted documents through the standard security handler: RC4 40 to 128 bit (revisions 2 to 4), AES-128 (`AESV2`) and AES-256 (`AESV3`, revisions 5 and 6), with per-object keys, crypt filters and decryption of both strings and streams. A document whose user password is empty — the common permission-only case — opens directly; otherwise the correct password is supplied through the CLI's bounded `--password-file` or the library API. The secret itself never appears in process arguments, output, traces or proof bundles.
- Document information from the trailer `/Info` dictionary, decoding both UTF-16BE and PDFDocEncoding strings. An absent dictionary is reported as unknown rather than filled in with the engine's own name.
- Page annotations: `/Link` annotations with a `/URI` action or a `/Dest` destination become hyperlinks with page-local geometry, and every other annotation subtype becomes an `Annotation` overlay carrying its `/Contents` text. Link targets are never fetched.

### Operations

Ingestion has a single boundary, `docsight-ingest`, which detects the format, dispatches to the parser and returns the normalized IR. Every operation below reads that IR. Format-specific types appear only where a raster or trace backend is genuinely required: PDF rasterization, PDF tracing, and the degraded `inspect` path for a PDF that cannot be fully converted.

- Navigation and structure: `inspect`, `outline`, `overview`, `peek`, `focus`, `context`, `page`.
- Text and tables: `text`, `tables`, `table` (JSON, Markdown, CSV, TSV, HTML).
- Resources and links: `images`, `links`. Link targets are reported, never fetched.
- Geometry: page-local points at 1/72 inch with the page origin at the top-left, used identically by geometry, render, crop, hit-testing and diff.
- Raster output: `render` (page PNG) and `crop` (page region or object region derived from its bounding box). Embedded PNG and JPEG images are decoded locally and drawn into their figure box; text, lines, boxes, table borders and fills are drawn from the same display list at any supported DPI.
- Search: `find` returns every literal or regex occurrence at the finest object granularity the IR holds — a table is searched cell by cell — with the matched character range, surrounding context, page, bounding box, source, confidence and the diagnostics that affect that object. Results can be narrowed by object kind, page range and a page region, and are bounded with deterministic continuation. Regular expressions run in guaranteed linear time.
- Spatial and structural selection: `query` (DQL with `above`, `below`, `inside`, `overlaps`, `nearest`, `distance-to`), `hit` (point or region to objects), `resolve` (ranked descriptor matching with explainable components).
- Object addressing is closed: every object identifier any command emits — block, table cell, nested block, overlay or hyperlink — is accepted by `evidence`, `context` and `crop`. An object without geometry of its own fails with a typed error that names the anchoring block to use instead.
- Evidence and reproducibility: `evidence`, `coverage`, `fingerprint`, `bundle`, `verify`, `replay`. Coverage reports text, structure, geometry, visual and resource fidelity per page and for the whole document, plus `unsupported_feature_count`, so a caller can tell how much of the document was actually interpreted.
- Comparison: `diff` at package, semantic and visual levels.
- Machine contract: `capabilities`, plus `--agent` JSON and `--ndjson` streaming with `--max-bytes`, `--max-items`, `--text-limit`, `--select`, `--budget` and deterministic continuation tokens. The discoverable `pdf_password` contract declares the file transport, 127-byte limit and fail-closed behavior for encrypted PDFs.
- Interactive setup: `completions` generates deterministic scripts for Bash, Elvish, Fish, PowerShell and Zsh. It is intentionally human-only and rejects agent or machine-output flags instead of mixing a shell script with JSON.
- Process isolation: `--sandbox` on Linux, macOS and Windows, failing closed on platforms where enforcement is unavailable.

### Diagnostics and errors

- Typed diagnostics with a stable code, severity, message, consequence and the affected object or page.
- Structured errors on stderr with the exit codes `0`, `2`, `10`, `11`, `12`, `13`, `20`, `21`, `30` and `40`, decidable without string matching.
- Documents whose password is unknown are rejected with exit code `12`; nothing is guessed.

---

## Partial

Behaviour in this section works, but is not source-faithful. Each item is reported at runtime through the listed diagnostic code, and reduces the corresponding fidelity dimension in `inspect` and `coverage`.

### DOCX

| Limitation | Diagnostic |
| --- | --- |
| Missing `sectPr`; a default Letter section is synthesized | `DOCX_SECTION_DEFAULTED` |
| Missing section geometry values use declared deterministic defaults | `DOCX_SECTION_GEOMETRY_DEFAULTED` |
| Binding gutters are parsed but not added to the body margins | `DOCX_SECTION_GUTTER_IGNORED` |
| Multi-column sections are laid out as one column | `DOCX_SECTION_COLUMNS_UNSUPPORTED` |
| A `nextColumn` section break starts a new page because multi-column flow is unsupported | `DOCX_NEXT_COLUMN_SECTION_UNSUPPORTED` |
| Widow/orphan control is relaxed only when the declared constraint cannot fit on an empty page | `DOCX_WIDOW_CONTROL_RELAXED` |
| Tables move as a whole because row-level table splitting is not implemented | `DOCX_PAGINATION_BLOCK_GRANULAR` |
| A block taller than the content area overflows the page | `DOCX_BLOCK_TALLER_THAN_PAGE` |
| A header or footer reference cannot be resolved to an available package part | `DOCX_HEADER_FOOTER_UNRESOLVED` |
| Unsupported header or footer fields retain their cached source result, which can be stale | `DOCX_HEADER_FOOTER_FIELD_CACHED` |
| Header and footer content is projected as deterministic plain text without source formatting, tab stops, tables, drawings, or positioned content | `DOCX_HEADER_FOOTER_LAYOUT_APPROXIMATED` |
| A header or footer extends into the body because body flow does not reserve additional space for it | `DOCX_HEADER_FOOTER_OVERLAPS_BODY` |
| A header or footer extends outside the physical page and is clipped | `DOCX_HEADER_FOOTER_OUTSIDE_PAGE` |
| Unsupported page-number formats render as decimal | `DOCX_PAGE_NUMBER_FORMAT_UNSUPPORTED` |
| Recognized DrawingML lines are preserved as `Shape` blocks, but their floating position, extent, stroke, and pixels are not projected | `DOCX_SHAPE_VISUAL_OMITTED` |
| Layout is computed rather than read from the source | `DOCX_LAYOUT_PAGINATED` |
| Text is measured with a deterministic proportional fallback font, not the document's own font | `DOCX_FONT_SUBSTITUTED` |
| An embedded image is neither a supported PNG nor JPEG, or its bytes fail strict decoding, so a placeholder box is rendered instead of its pixels; the diagnostic names the detected format or decoding failure | `DOCX_FIGURE_RASTER_PLACEHOLDER` |
| An image relationship cannot be resolved | `DOCX_IMAGE_UNRESOLVED` |
| Numbering definitions or formats cannot be resolved | `DOCX_NUMBERING_UNRESOLVED`, `DOCX_NUMBERING_FORMAT_MISSING` |
| Unusable table grid widths fall back to equal columns | `DOCX_TABLE_GRID_WIDTHS_UNUSABLE` |
| Run-level elements that are not interpreted, so paragraph text may be incomplete | `DOCX_RUN_ELEMENT_UNSUPPORTED` |
| Body elements preserved as opaque nodes without semantic interpretation | `DOCX_BODY_ELEMENT_UNSUPPORTED` |
| Embedded objects and active content preserved inert, digest only | `DOCX_EMBEDDED_OBJECT_INERT`, `DOCX_ACTIVE_CONTENT_INERT` |
| A hyperlink target page cannot be resolved | `DOCX_LINK_PAGE_UNRESOLVED` |
| Contextual spacing is declared but not applied, so spacing is added even between paragraphs of the same style | `DOCX_CONTEXTUAL_SPACING_IGNORED` |


Tracked changes are counted, not reconstructed: `tracked_changes` reports insertion and deletion counts without per-revision authorship or content.

### PDF

| Limitation | Diagnostic |
| --- | --- |
| An image XObject is placed as a figure box; its pixels are not decoded | `PDF_XOBJECT_PLACEHOLDER` |
| A page paints only images and carries no text operators, so there is nothing to extract without OCR | `PDF_PAGE_HAS_NO_TEXT_LAYER` |
| A shading resource (`sh`) is not painted | `PDF_SHADING_UNSUPPORTED` |
| Text without an embedded outline uses the deterministic fallback glyph set | `APPROXIMATED_PDF_FONT` |
| Codes the `ToUnicode` CMap does not map are extracted as the replacement character | `PDF_TEXT_CODE_UNMAPPED` |
| Unsupported `ExtGState` entries are ignored for painting | `PDF_EXTGSTATE_IGNORED` |
| Soft masks, unsupported blend modes, patterns and unsupported colour spaces are ignored for painting | `PDF_SOFT_MASK_IGNORED`, `PDF_BLEND_MODE_UNSUPPORTED`, `PDF_PATTERN_PAINT_UNSUPPORTED`, `PDF_COLOR_SPACE_UNSUPPORTED` |
| Clipping text rendering modes extract text but do not clip | `PDF_CLIP_TEXT_VISUAL` |
| Negative font sizes extract text but do not reproduce the signed transform | `PDF_NEGATIVE_FONT_SIZE_VISUAL` |
| Strokes under a non-uniform transform use an area-preserving mean width | `PDF_NON_UNIFORM_STROKE_VISUAL` |

Structure is inferred, never authoritative: PDF paragraphs, headings and tables always carry a confidence value, and tables also carry the detector that produced them. Unsupported content operators and unsupported font programs fail closed with exit code `20` rather than guessing; features that only affect painting reduce visual fidelity and are reported, but never veto text or structure.

### Operation-level diagnostics

These are not format limitations. They are the cases where a command can answer, but not completely, and says so.

| Limitation | Diagnostic |
| --- | --- |
| PDF structure is reconstructed, so blocks and tables are reported as inferred | `INFERRED_SEMANTICS`, `INFERRED_TABLE` |
| A semantic diff item rests on a reconstruction below full confidence | `LOW_CONFIDENCE_RECONSTRUCTION` |
| Visual diff evidence is limited because the compared pages are not source-faithful | `DIFF_VISUAL_EVIDENCE_LIMITED` |
| Cross-version lineage cannot be resolved to a single predecessor | `DIFF_LINEAGE_AMBIGUOUS` |
| A crop region extends past the object's assigned page and is clipped to it | `OBJECT_CROP_CLIPPED_TO_PAGE` |
| A render fingerprint could not be computed for an object | `RENDER_FINGERPRINT_UNAVAILABLE` |
| A trace does not carry complete decision data, so replay verifies what is available | `TRACE_DECISION_PARTIAL` |
| An object has no canonical geometry, so it is excluded from spatial results and counted | `SPATIAL_GEOMETRY_UNAVAILABLE` |
| `context` cannot assign a page to one canonical DOCX section | `CONTEXT_SECTION_UNAVAILABLE` |
| `context` has no object-level fidelity for an overlay | `CONTEXT_FIDELITY_UNAVAILABLE` |

### Images

PNG is decoded at 8 bits per channel, non-interlaced, in greyscale, RGB, palette, greyscale with alpha or RGBA. JPEG is decoded strictly through a pinned pure-Rust adapter with platform-specific acceleration disabled, so malformed or non-conformant data is not accepted as visual evidence. GIF, BMP, TIFF, EMF, WMF and SVG parts are preserved with their digest and reported as placeholders. Scaling to the figure box is nearest-neighbour, which is deterministic but does not filter; enlarging a small image shows its pixels rather than a smoothed version.

### Cross-cutting

- Recognized DrawingML lines produce `Shape` blocks. Other Office shapes and `Watermark` overlays are not produced yet.
- Headers, footers and comment markers are DOCX-only and come from layout; `Annotation` overlays are PDF-only and come from page annotations.
- Annotation appearance streams are not rendered. An annotation contributes its geometry and text to the IR, not its pixels.
- Resource coverage counts figures and declared resources. A PDF whose content lives in an untraversed Form XObject therefore reports reduced resource coverage rather than silently reporting none.
- The `caption` DQL selector is DOCX-only and fails closed for PDF, because caption semantics are not reconstructed there.
- There is no cache between invocations. Every command re-ingests the document.

---

## Out of scope

These are not limitations to be fixed inside the current product. They are deliberate boundaries.

- **OCR and image understanding.** DocSight never infers text from pixels.
- **Document editing or authoring.** No writing, no conversion back to DOCX or PDF, no redaction.
- **Other Office formats.** No XLSX, no PPTX, no legacy binary `.doc`/`.xls`/`.ppt`.
- **Active content execution.** Macros, OLE objects, PDF JavaScript, embedded attachments and form actions are preserved inert or rejected; they are never executed.
- **Network behaviour.** No daemon, no server mode, no cloud rendering, no telemetry, no hyperlink fetching, no remote font or resource resolution.
- **External document engines.** No Word, LibreOffice, Excel, COM automation, `unoconv` or remote converter as a runtime dependency; no MuPDF or other third-party document interpreter in the binary. MuPDF may be used only as an optional external oracle during development.
- **Retrieval and question answering.** No RAG, no vector search, no embeddings, no natural-language queries over documents.
- **Word-identical rendering.** DocSight measures and publishes the fidelity it achieves; it does not claim to reproduce Word's or Acrobat's output pixel for pixel.
- **Password recovery.** Decryption requires the correct password, or an empty user password where the document permits it. No guessing, dictionary search or owner-password circumvention.

---

## Related documents

- `BACKLOG.md` — what is deferred, split into v1, post-v1 and experimental.
- `AGENT_PROTOCOL.md` — the machine contract in detail.
- `AGENTS.md` — engineering rules for changing any of the above.
