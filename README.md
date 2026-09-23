# DocSight

> **Status:** pre-release / active development — no stable binary release yet.

DocSight is a local, headless evidence layer for inspecting DOCX and PDF files from a terminal. It gives agents synchronized structural, textual and visual views without Word, LibreOffice, COM automation, a remote service or network access.

The same document bytes, engine version, backends, fonts and options produce deterministic JSON and artifacts. Unsupported or approximate behavior is exposed through typed diagnostics rather than hidden behind a fallback.

## Installation

For a maintainer-published binary candidate, follow the [installation guide](INSTALL.md).
A source checkout or a workflow definition does not mean binaries have already
been published. The configured five-target matrix requires native validation
before a candidate is described as supported. Running the extracted binary does
not require the Rust toolchain.

## Five-minute agent-first quickstart

For development or when no binary has been published, build with the pinned stable Rust toolchain:

```bash
cargo build --locked --release
export PATH="$PWD/target/release:$PATH"
export DOC="/absolute/path/to/document.docx"
```

Discover the live machine contract instead of parsing help text:

```bash
docsight --agent capabilities
```

Start with a bounded map of the document:

```bash
docsight --agent --budget 4kb overview "$DOC"
docsight --agent --budget 8kb peek "$DOC" --page 1
```

Ask for a focused context package when the target is known by description:

```bash
docsight --agent --budget 12kb context "$DOC" --find "quarterly revenue" --kind table --include content,heading,geometry,fidelity,provenance
```

Use `find` when every occurrence matters or exact filters are needed:

```bash
docsight --agent --max-items 20 find "$DOC" "revenue" --ignore-case --kind heading,table-cell
```

Take an `object` value returned by `context`, `find` or `peek`, then request provenance or visual evidence:

```bash
export OBJECT="obj_replace_with_returned_id"
docsight --agent evidence "$DOC" "$OBJECT"
docsight --agent crop "$DOC" --object "$OBJECT" --out evidence.png
```

Successful agent calls write only JSON or NDJSON to stdout. Errors are structured on stderr, leave stdout empty and use the documented exit code. Add `--sandbox` for hostile input after checking the platform policy returned by `capabilities`.

For a password-protected PDF, `--password-file` is recommended so secrets do not leak into shell history or process listings. Pass only the path to a private single-line file:

```bash
install -m 600 /dev/null pdf-password.txt
read -r -s PDF_PASSWORD
printf '%s' "$PDF_PASSWORD" > pdf-password.txt
unset PDF_PASSWORD
docsight --agent --password-file pdf-password.txt inspect "$DOC"
```

DocSight also accepts `--password PASSWORD` directly when command-line arguments are safely isolated.

The password is limited to 127 bytes, cleared from the CLI's memory when the operation finishes and never written to JSON, traces or proof bundles. Replay and proof verification for encrypted source bytes accept `--password-file` or `--password`. A wrong or absent password fails with exit code 12; DocSight never guesses it.

## Command surface

`docsight --agent capabilities` is authoritative for invocation grammar, formats, output modes and result schemas. The commands are:

| Area | Commands | Purpose |
| --- | --- | --- |
| Discovery | `capabilities`, `inspect`, `coverage`, `fingerprint` | Discover the contract, format, fidelity and reproducibility inputs. |
| Navigation | `overview`, `peek`, `focus`, `context`, `resolve` | Move from a bounded document map to one explainable semantic target. |
| Content | `outline`, `text`, `page`, `images`, `links` | Read normalized structure, text, page geometry and resources. |
| Tables | `tables`, `table` | List tables or export one as JSON, Markdown, CSV, TSV or HTML. |
| Retrieval | `find`, `query`, `hit` | Locate literal or regex matches, run spatial DQL and resolve coordinates. |
| Visual evidence | `render`, `crop`, `evidence` | Produce deterministic PNG evidence and its provenance record. |
| Portable evidence | `bundle`, `verify`, `replay` | Create and verify proof bundles or deterministic render traces offline. |
| Comparison | `diff` | Compare package, semantic and visual changes with lineage. |
| Caching | `cache` | Report, verify, prune or clear the opt-in document IR cache. |
| Interactive setup | `completions` | Generate a completion script for Bash, Elvish, Fish, PowerShell or Zsh. |
| Agent integration | `mcp` | Run Model Context Protocol (MCP) server on stdio. |

Use `docsight <command> --help` for human-readable flags. Use the machine contract for integrations; command names, schemas, units, ordering and exit codes are public contracts.

## Reusing parsed documents

An agent that runs several commands against the same document can keep the parsed IR in a private cache directory:

```bash
docsight --agent --cache-dir .docsight-cache overview "$DOC"
docsight --agent --cache-dir .docsight-cache find "$DOC" "termination"
docsight --agent --cache-dir .docsight-cache cache stats
```

The cache is off unless `--cache-dir` is passed. Entries are keyed by the document bytes and the exact DocSight executable, written atomically and fully validated before reuse; an invalid entry is quarantined and the document is parsed again. Results are byte-identical with and without the cache, including under `--sandbox`, where the parent process owns the directory. Only commands that read the document IR use it; `--password-file` and `--password` cannot be combined with it. Entries contain document content, so keep the directory private and clear it with `cache clear` when it is no longer needed. Use separate cache directories for documents you trust and documents you do not.

## Bounded output

Agent calls support hard output controls:

- `--max-bytes`, `--max-items` and `--text-limit` cap serialized results.
- `--select` projects result fields.
- `--budget` and `--budget-profile` choose adaptive JSON projections.
- `--continue` resumes a truncated collection with a deterministic token.
- `--ndjson` streams records while preserving typed metadata and completion records.

Truncation is explicit. DocSight never silently drops evidence to fit a limit.

## Shell completion

Generate the script from the same Clap command model used by the executable:

```bash
docsight completions bash > docsight.bash
docsight completions zsh > _docsight
docsight completions fish > docsight.fish
docsight completions powershell > docsight.ps1
docsight completions elvish > docsight.elv
```

`completions` is a human-only utility because its stdout is shell source. Combining it with `--agent`, `--ndjson` or machine-output limits fails with exit code 2.

## Contract and scope

- [Product scope](PRODUCT_SCOPE.md) defines what is supported, partial and out of scope.
- [Agent protocol](AGENT_PROTOCOL.md) defines JSON, NDJSON, limits, continuation and error behavior.
- [Backlog](BACKLOG.md) separates v1 work from post-v1 and experimental ideas.
- [Fuzzing guide](FUZZING.md) documents parser fuzz targets.
- [Performance and scale](PERFORMANCE.md) documents the versioned benchmark workloads, metrics and CI budgets.

DocSight does not edit documents, perform OCR, answer natural-language questions, fetch hyperlinks or run as a daemon. PDF structure is inferred with confidence and provenance; DOCX layout reports every approximation that can affect evidence. Raster images are supported for DOCX (PNG, JPEG) and PDF Image XObjects (DCTDecode/JPEG, 8-bit FlateDecode in DeviceRGB/DeviceGray), with unsupported formats emitting explicit typed diagnostics (`PDF_XOBJECT_PLACEHOLDER`, `DOCX_FIGURE_RASTER_PLACEHOLDER`) and failing closed rather than guessing. Scanned documents without a text layer continue to report `PDF_PAGE_HAS_NO_TEXT_LAYER`.

## Maintainer validation and beta

[Release procedures](RELEASE.md) document native packaging, checksums, smoke
checks and the V1 evidence gate. [Beta procedures](BETA.md) cover local opt-in
observations, consent and reproducing a failure before fixing it. Neither a
passing automation unit test nor an empty issue register is release acceptance.
The engine remains at version 0.1.4; no v1-blocking engine gaps remain open in the release registry, while the other readiness criteria still require native validation evidence.

## License

DocSight is dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
