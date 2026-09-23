# Security Policy

## Supported Versions

DocSight is under active pre-release development. Security fixes are applied to the latest development version on the default branch and the current release series:

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1.0 | :x:                |

## Security Model and Guiding Principles

DocSight is designed as a local, headless evidence layer for inspecting DOCX and PDF documents. It operates under a hostile-input security model: every document file processed by DocSight is treated as potentially malicious untrusted data.

### Core Defenses

- **Strict Offline Operation:** DocSight never requires, initiates, or performs network requests during parsing, inspection, or rendering. External hyperlinks found in documents are reported as text/metadata, never fetched.
- **No Active Content Execution:** Embedded macros, OLE objects, PDF JavaScript actions, executable launch actions, and file attachments are strictly rejected or treated as inert data.
- **Resource Limits and Denial-of-Service Defenses:** Ingestion applies strict resource bounds against archive bombs (zip bombs), decompression ratios, excessive XML depth and token sizes, maximum page counts, and unbounded memory allocations.
- **Magic Byte Verification:** Document formats are identified by magic byte signatures; file extensions are never used as a trust indicator.
- **Safe Memory and Isolated Execution:** The core engine is built in memory-safe Rust with `unsafe_code` denied across the workspace. For untrusted hostile input, DocSight provides an optional `--sandbox` mode that delegates parsing to an isolated child worker process constrained by OS-level sandbox primitives and strict filesystem access policies.

## Password and Secret Handling

For password-protected PDFs:

- `--password-file` is recommended over direct command-line arguments to prevent passwords from being exposed in shell history, process listings, or command line inspection. Direct `--password` is supported when invocation environments are safely isolated.
- Passwords are restricted to a maximum length of 127 bytes.
- Passwords are held only in transient memory during processing, cleared immediately after use, and are never written to stdout, JSON/NDJSON records, proof bundles, or execution traces.
- Password-protected documents cannot be cached.

## Reporting a Vulnerability

If you discover a security vulnerability in DocSight, please do not open a public issue. Instead, report it privately through GitHub Security Advisories:

1. Navigate to the repository Security tab: [Security Advisories](https://github.com/GustavoMartins123/DocSight/security/advisories).
2. Click **Report a vulnerability** to open a private advisory draft.

Alternatively, contact the repository maintainer directly via the email listed on their GitHub profile.

### Report Details

To help triage and address the report quickly, please include:
- A clear description of the vulnerability and its potential security impact.
- The affected DocSight version(s) and operating system platform.
- A minimal reproducing sample document or proof of concept.
- Any relevant crash logs, backtraces, or diagnostic output.

### Coordinated Disclosure

We are committed to coordinated vulnerability disclosure:
- We acknowledge receipt of vulnerability reports within 48 hours.
- We will work with the reporter to investigate, reproduce, and patch the issue in a timely manner.
- Security fixes will be released in an advisory release before public disclosure.
