# Changelog

## Unreleased

- Replace all permanent DS9-DS12 interpreter tooling and auxiliary tests with native Rust modules and integration tests in xtask.
- Route release, notices, changelog, smoke, corpus, beta, validation and readiness through cargo xtask and guard the Rust-only architecture in CI.
- Require both docsight and docsight-worker in every native archive.
- Version native validation receipts as v2 with explicit pass, fail, unavailable, blocked and not_run states and nullable unavailable exit codes.

These changes implement candidate tooling; they do not claim a completed native
validation campaign, real beta, signed release or V1 acceptance.

- Gate five native targets on locked tests/builds, extracted-binary smoke checks and the synthetic corpus.
- Generate deterministic Git-range release notes and locked dependency license notices.
- Collect opt-in local beta metadata without document content, paths, passwords or automatic upload.
- Validate hash-pinned corpus cases, paired-document diffs, typed errors and repeated output bytes.
- Detect documents changed during the final corpus command, not only between repeats.
- Retain all validation gates and distinguish failed tests from unavailable compilers.
- Evaluate candidate-bound V1 evidence and preserve open engine gaps and required human reviews.
- Version maintainer evidence schemas separately from the public agent v2 contract.
- Reject JSON numeric overflow, relabeled crash reports, symlinked license resources and corrupt archive streams.
- Preserve Windows static CRT build flags and ship all IR schemas and referenced offline guides.
- Document installation, real beta consent, before-fix regressions and release review procedures.

- Add a pinned DS9 benchmark runner and locked dependency enforcement.
- Add deterministic release packaging, target validation and SHA-256 verification.
- Add installation instructions and the workspace-declared dual license texts.

## 0.1.4

Existing source version at the start of DS9-DS12 continuation. This entry does not
claim that a binary release or stable V1 has been published. The Git history is
the detailed record of earlier engine and CLI changes.
