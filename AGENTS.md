# AGENTS.md

## Scope and Source of Truth

- Read `Projeto_DOCSIGHT_Especificacao.docx` (the project specification) before making architectural decisions or modifying public contracts.
- Treat the project specification as the product source of truth. In case of conflict, follow this precedence order: current explicit user request, this `AGENTS.md`, project specification, and existing conventions in the codebase.
- Preserve the core objective: DOCSIGHT is a local, headless evidence layer for inspecting DOCX and PDF from a terminal without depending on Word, Excel, LibreOffice, COM automation, or remote services.
- Do not expand scope unilaterally. DOCX editing, XLSX, PowerPoint, Office chart reflow, natural language questions, and daemon/network modes are strictly out of scope for v1.

## Mandatory User Rules

- Do not add comments to code without explicit user request. This includes line comments, block comments, explanatory comments, TODOs, FIXMEs, and doc comments. Prefer precise names, small functions, clear types, and external documentation when requested.
- Never execute `git commit`, `git push`, create tags, publish releases, open pull requests, or rewrite history without explicit user authorization for that specific action.
- Do not create or maintain functional fallbacks. Do not add legacy aliases, alternative routes, silent backwards compatibility, placeholder values, automatic degradation, or secondary execution paths.
- If the canonical path fails, return an explicit error, preserve diagnostics, and operate in fail-closed mode.
- Remove existing fallbacks when they are within the scope of the task. Transactional rollbacks to restore previous state after failure remain permitted.
- Do not hide unsupported features, approximations, loss of evidence, or reduced fidelity. Expose typed diagnostics with consequence and affected objects.
- Do not modify files unrelated to the task. Preserve existing user changes and never discard work without authorization.

## Product Principles

- Provide three synchronized document views: structural, textual, and visual.
- Normalize DOCX and PDF into a single immutable `Document IR`. After ingestion, commands and services must not access format-specific structures directly.
- Treat text, layout, geometry, relationships, resources, and rendered evidence as first-class data.
- Use PDF points (1/72 of an inch), page-local coordinates, and top-left origin as the public geometric coordinate system.
- Generate deterministic IDs from document digest, origin identity, and normalized semantic path. Never use random UUIDs, memory addresses, thread scheduling order, or unstable global state.
- For the same bytes, engine version, backends, fonts, and options, produce byte-identical JSON and artifacts.
- Maintain local and offline execution as an absolute rule. Parsing, inspection, and rendering must never require network access.
- Do not fetch external hyperlinks or execute macros, OLE, or embedded active content.
- Distinguish structural facts from inferences. Every inferred result must expose confidence and provenance.

## Expected Architecture

Use a Rust workspace with clean separation of concerns and no circular dependencies:

- `docsight-cli`: `clap` CLI interface, argument validation, and human presentation.
- `docsight-agent`: JSON/NDJSON schemas, output limits, and continuation tokens.
- `docsight-core`: IDs, geometry, `Document IR`, provenance, and diagnostics.
- `docsight-ingest`: Single ingestion boundary; sniffs format, dispatches to parser, returns normalized `Document IR`.
- `docsight-ooxml`: OPC/ZIP reading and resource-bounded OOXML parsing.
- `docsight-layout`: Deterministic pagination and layout for DOCX.
- `docsight-pdf`: Custom PDF parser, page tree, resources, content streams, and normalization into the IR.
- `docsight-render`: Display lists, rasterization, crop, and contact sheets.
- `docsight-fonts`: Font discovery, shaping, and deterministic fallback resolution.
- `docsight-tables`: Canonical table model, PDF table inference, and exporters.
- `docsight-search`: Text search, regex, and future DQL.
- `docsight-diff`: Package, semantic, and visual diff with lineage.
- `docsight-cache`: Content-addressed cache and reproduction fingerprinting.
- `docsight-worker`: Isolation worker for parsers and native backends.
- `fixtures`: Minimal, adversarial, and golden test documents.
- `schemas`: Versioned snapshots of public machine contracts.
- `xtask`: Build automation, release tasks, and explicit golden updates.

Do not create empty crates prematurely. Introduce each crate only when there is a concrete responsibility and corresponding tests. External library types must never leak into the IR or public schemas; encapsulate them within adapters.

## Implementation Order

- Follow milestones M0 through M9 and the first 15 issues described in the specification.
- Begin with a minimal verifiable increment: magic byte sniffing, typed errors, core structures, bounded OPC, basic DOCX semantics, basic PDF, and only then progressively broader layout.
- For the initial DOCX layout, deliberately support a single section, single font, paragraphs, and explicit page size. Require deterministic geometric snapshots and PNG rendering before expanding the OOXML surface.
- Deliver complete vertical slices: contract, implementation, diagnostics, tests, and requested documentation. Avoid speculative scaffolding.
- Do not anticipate DQL, OCR, vector search, or other plugins before a stable interface and milestone requirement exist.

## Rust Coding Practices

- Use stable Rust and keep the minimum supported version declared in the workspace.
- Format with `cargo fmt` and ensure `cargo clippy --all-targets --all-features` produces zero warnings before completing a change.
- Prefer domain types, exhaustive enums, newtypes, and validated invariants over loose strings, generic maps, and ambiguous booleans.
- Keep functions small, cohesive, and single-purpose. Separate parsing, validation, normalization, layout, presentation, and I/O.
- Avoid duplication, mutable global state, implicit side effects, and premature abstractions.
- Propagate errors with typed context. Do not use `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` in production paths.
- Do not ignore `Result`, compiler warnings, unknown elements, or potentially truncating conversions.
- Use checked arithmetic and verified conversions for sizes, offsets, indices, and boundary bounds. Treat overflow as an explicit error.
- Restrict `unsafe` to the smallest possible module, preferably auditable FFI wrappers. Expose a safe Rust API and test invalid inputs and backend failures.
- Concurrency must operate only on independent units. Order results and diagnostics canonically before emitting them to maintain byte-stable outputs.
- Keep public APIs minimal. Changes to schemas, IDs, coordinates, exit codes, or ordering require contract tests and explicit decisions.

## Dependencies and Build

- Add dependencies only when there is a concrete necessity, verifying maintenance, licensing, attack surface, and cross-platform support.
- Pin native backends and components whose behavior affects rendering or determinism. Track `Cargo.lock` for the binary.
- Do not introduce Word, LibreOffice, Excel, COM, `unoconv`, remote converters, or network calls as runtime dependencies.
- Do not add MuPDF or another document interpreter/renderer as a runtime dependency. The PDF engine and authoritative renderer are DocSight's own implementations.
- MuPDF may only be used as an optional, explicit external oracle in development tests, without integrating into the workspace, binary, or normal test runner.
- Fallback fonts must be deterministic and declared in diagnostics. Never silently select an arbitrary system font.
- Build and tests must succeed on Windows, Linux, and macOS, respecting filesystem differences without altering observable contracts.

## Parsing and Security

Treat every document as hostile input.

- Detect format by magic bytes; never rely solely on file extensions.
- Limit entry count, compressed and uncompressed size, compression ratio, XML depth, token sizes, pages, images, memory, CPU, and layout iterations.
- Disable external XML entities (XXE) and external resource resolution.
- Normalize OPC paths and reject absolute paths, traversal (`..`), invalid relationships, and ambiguous collisions.
- Validate dimensions and decoded byte buffers before allocating image memory.
- Preserve unknown OOXML elements as opaque nodes linked to the nearest object and emit diagnostics. Do not claim full interpretation.
- Isolate the PDF backend and native decoders inside a worker process when security policy demands it. Convert worker failures into typed errors.
- Never execute macros, OLE objects, PDF JavaScript, file attachments, or active content.

## Document IR Contract

- The IR is immutable once the corresponding phase is frozen and preserves provenance for every object.
- Visible blocks must carry, when knowable: ID, type, page, `bbox`, z-index, reading order, origin, and confidence.
- Explicitly model document, metadata, styles, sections, pages, blocks, overlays, and resources.
- Preserve paragraphs, headings, list items, tables, figures, shapes, hyperlinks, bookmarks, comments, footnotes/endnotes, tracked changes, and supported fields.
- Do not discard information to simplify export. Export a reduced form only when explicitly requested and with documented loss.
- DOCX tables are structural: preserve grid, spans, nested content, styles, coordinates, and geometry. PDF tables are inferred and always expose confidence and detector identity.

## Layout and Rendering

- Implement layout as deterministic phases: section geometry, fonts, shaping, lines, paragraphs, lists, tables, floating objects, pagination, headers/footers, and final freeze.
- Respect page breaks, `keep-with-next`, `keep-lines`, widows/orphans, columns, spans, and line breaks within declared coverage.
- Associate each approximation with a diagnostic code, probable consequence, reduced confidence, and affected objects.
- Use identical page transformations for geometry, rendering, cropping, hit-testing, and diffing.
- Object cropping must derive coordinates directly from the bounding box without requiring callers to estimate pixels.
- Do not claim Word fidelity where it does not exist. Measure and expose achieved fidelity explicitly.

## CLI and Agent Protocol

- Treat `stdout` as an API. In agent mode, emit only valid JSON or NDJSON, with zero decoration, progress bars, or ANSI escapes.
- Send diagnostics and progress exclusively to `stderr`. `--quiet` must suppress them when applicable.
- Maintain explicitly versioned schemas validated against JSON Schema.
- Guarantee canonical ordering by page, reading order, and ID.
- Large operations must provide NDJSON streaming and rigid limits such as `--max-bytes`, `--max-items`, and `--text-limit`, with explicit truncation and deterministic continuation tokens.
- Never silently alter field names, semantics, units, IDs, exit codes, or error structures.
- Use the exit codes defined in the specification: `0`, `2`, `10`, `11`, `12`, `13`, `20`, `21`, `30`, and `40` for their corresponding categories.
- Errors must enable programmatic decision-making without string parsing and include sufficient context for remediation.
- Human mode is a projection of the same typed records; never implement divergent product logic in the presentation layer.

## Cache and Reproducibility

- Key cache entries by exact file digest, engine version, backend versions, font fingerprint, layout profile, and relevant options.
- Never reuse a cached result with an incompatible fingerprint.
- Cache writes must be atomic. Corruption or incompatibility must produce an explicit error or explicit invalidation, never silent use of suspect data.
- Never include timestamps, local absolute paths, or execution thread order in deterministic output, unless explicitly required by contract in a cleanly separated field.

## Testing and Completion Criteria

Every change must be tested at the appropriate layer.

- Unit tests for parsing, IDs, geometry, normalization, limits, and errors.
- Property tests for OPC, paths, relationships, style cascading, and structures exposed to adversarial combinations.
- Minimal fixtures for every feature and malformed fixtures for every defense mechanism.
- Deterministic snapshots for IR, schemas, CLI output, and geometry.
- Visual goldens with explicit rendering tolerances; never update goldens merely to make tests pass without investigating the diff.
- Integration tests for stdout/stderr separation, exit codes, limits, continuation, and offline execution.
- Repeated parallel executions must produce byte-identical JSON.
- Fuzzing for OPC container, OOXML relationships, styles, numbering, table grid, paragraph layout, PDF span clustering, and DQL parser when present.
- Test limits immediately above and below allowed thresholds, not just the happy path.
- A change is complete only when formatting, linting, relevant tests, and affected contracts pass. If a test cannot be executed, report the exact command, reason, and what remained unvalidated.

## Agent Workflow

1. Read the user request, this file, relevant parts of the specification, and files involved before editing.
2. Check the current working directory state and preserve user changes.
3. Define the smallest increment that completely resolves the request.
4. Identify contracts, security risks, determinism, limits, and affected tests.
5. Implement using the canonical path, with no fallbacks and no code comments.
6. Run `cargo fmt`, `cargo clippy --all-targets --all-features`, targeted tests, and when justified by risk, `cargo test --workspace --all-features`.
7. Review final diff to detect accidental modifications, external type leakage, non-deterministic output, alternative paths, and stray generated files.
8. Objectively report what changed, which verifications passed, and any real limitations. Do not commit.

## Conduct When Modifying Contracts

- Do not preserve legacy contracts via aliases or silent compatibility. If a breaking change is requested, update the canonical path and all consumers within scope.
- Public schemas require explicit versioning. A new version does not authorize two implicit routes; selection must be deliberate.
- Changes to IDs, geometry, ordering, serialization, diagnostics, limits, and cache require dedicated fixtures and snapshots.
- If a decision contradicts the specification, stop and request user authorization before implementing.

## Documentation and Communication

- Maintain names, error messages, CLI help, and schemas in English, consistent with the specification, unless instructed otherwise.
- Write external documentation only when part of the task or required for an altered public contract.
- Do not use code comments as a substitute for a clear API.
- Upon completing substantial work, briefly mention only genuinely relevant improvements that could still be made in architecture, performance, or optional Docker services. Treat them as recommendations and do not implement without a request.
