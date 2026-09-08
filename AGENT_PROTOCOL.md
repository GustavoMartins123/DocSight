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

Every document result carries a deterministic document ID and SHA-256 digest. Use `warnings`, `capability_details`, and `coverage` before treating geometry or pixels as authoritative. `source_faithful` distinguishes an available operation from an exact representation.

Output limits are explicit. `limits.truncated` describes omitted result items, while `limits.warnings_truncated` describes omitted diagnostics. A continuation token is valid only for the command and document that produced it.

Render and crop results include the requested output path, PNG media type, byte count, SHA-256 digest, page bounding box, and pixel dimensions. The digest verifies the artifact; fidelity still comes from the warnings and coverage records.

Agent errors use `schemas/v2/error-envelope.json` and include a stable diagnostic code and process exit code. Exit code `0` is success; non-zero codes must be handled programmatically without matching human messages.
