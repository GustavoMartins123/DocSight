# Native maintainer tooling

DocSight product code, tests and permanent maintenance implementation are Rust.
`cargo xtask` is the single maintenance entry point. It uses the workspace
lockfile and does not delegate product logic to another language. A built xtask
binary can run commands directly; the existing benchmark subcommand is retained.

## Commands and modules

| Command or capability | Rust implementation | Rust integration coverage |
| --- | --- | --- |
| Strict JSON, bounded IO, hashing, safe paths, exclusive publication | `xtask/src/tooling/common.rs` | `tooling_common.rs` |
| Deadline, combined stdout/stderr budget, process-tree cleanup | `xtask/src/tooling/process.rs` | `tooling_process.rs` |
| `release matrix`, `configuration`, `version`, `package`, `verify`, `collect` | `xtask/src/release/{mod,archive,binary}.rs` | `tooling_release.rs`, `tooling_cli.rs` |
| `release signature ARCHIVE --out FILE`, embedded Mach-O and PE signature inspection, `release/distribution-policy.json` | `xtask/src/release/{signature,distribution}.rs` | `tooling_signature.rs`, `tooling_distribution.rs` |
| `release provenance ARCHIVE --out FILE` | `xtask/src/release/provenance.rs` | `tooling_provenance.rs` |
| `release lifecycle --previous ARCHIVE --candidate ARCHIVE --out FILE` | `xtask/src/release/lifecycle.rs` | `tooling_lifecycle.rs` |
| `notices --metadata FILE --out FILE` | `xtask/src/release/notices.rs` | `tooling_notices.rs` |
| `changelog --revision SHA --since SHA --out FILE` | `xtask/src/release/changelog.rs` | `tooling_changelog.rs` |
| `smoke ARCHIVE --out FILE` | `xtask/src/smoke.rs` | `tooling_smoke.rs` |
| `corpus validate` and `corpus run --archive ARCHIVE --out FILE`, with `--classes FILE` for the document class taxonomy | `xtask/src/corpus/{manifest,runner}.rs` | `tooling_corpus.rs` |
| Document class taxonomy and per-class signals | `release/document-classes.json`, `xtask/src/quality/classes.rs` | `tooling_quality.rs` |
| `quality prepare --archive ARCHIVE --document FILE --class CLASS --out FILE`, `quality validate`, `quality measure --archive ARCHIVE --out FILE` and `quality compare --baseline FILE --candidate FILE` | `xtask/src/quality/{engine,ground_truth,measure,compare}.rs` | `tooling_quality.rs`, `tooling_contracts.rs` |
| `beta collect --consent` and `beta aggregate DIRECTORY` | `xtask/src/beta.rs` | `tooling_beta.rs` |
| `validate --out DIRECTORY` | `xtask/src/validation.rs` | `tooling_validation.rs` |
| `readiness --revision SHA --evidence DIRECTORY` | `xtask/src/readiness/{mod,evidence,campaigns,reviews}.rs` | `tooling_readiness.rs` |
| `rust-only` | `xtask/src/architecture.rs` | `tooling_architecture.rs` |
| Open V1 engine gaps keep readiness blocked | `release/known-gaps.json` | `tooling_readiness.rs` |
| DS9 benchmark budgets and generated scale fixtures | `xtask/src/main.rs`, `benchmarks/` | unit tests in `xtask/src/main.rs` |
| Evidence schema conformance of generated artifacts, pinned workflow toolchain, locked Cargo commands, explicit `xtask` binary selection, target-filtered notices metadata | `schemas/tooling/v2/evidence.json`, `.github/workflows/`, shipped guides | `tooling_contracts.rs` |

Use `cargo xtask --help` and each command's `--help` for complete options. The
global `--root` selects an explicit checkout or corpus root. Relative file paths
are interpreted from the current invocation directory unless resolved under a
manifest root by the command. `release configuration` writes the validated
matrix and version to `GITHUB_OUTPUT` when present; it does not execute arbitrary
configuration or mutate the repository.

## Operational boundaries

Input JSON rejects duplicate object keys, trailing data, non-finite numbers and
excessive nesting. File reads, decompression, member counts, license traversal,
process execution and numeric conversions have explicit limits. JSON output is
deterministic and ASCII escaped. Evidence files are published through temporary
files without overwriting existing receipts. Hashes bind actual bytes, not a
path guessed from a filename. Symlinks, device paths and traversal are rejected
where inputs must remain under trusted roots.

Subprocesses use argument vectors, not shell command construction. Unix process
groups and Windows Job Objects bound child lifetime. The outer runner is not a
complete operating-system sandbox: document operations additionally use the
existing `--sandbox` engine path. Isolated smoke and beta environments do not
inherit arbitrary host variables. Windows directory ACLs remain an operator
responsibility. Do not run a candidate from an untrusted publisher just because
its checksum is internally consistent.

The release verifier requires both the CLI and worker and all required schemas,
licenses and offline guides. Signature and provenance receipts follow the
distribution policy; while its entries are pending the candidate remains
unsigned and readiness says so. Notices validate
resolved package identity against Cargo.lock and omit host paths; maintainers
still review redistribution obligations. Changelog references are full commit
identifiers with checked ancestry. No maintenance command force-pushes, merges,
creates tags, publishes releases or invents consent.

## Testing and evidence

Run `cargo test --locked -p xtask --all-targets` for maintenance coverage and
`cargo test --locked --workspace --all-features` for the full workspace. The
workspace contains two xtask binaries: `maint`, selected by the `cargo xtask`
alias, and `xtask`, which owns the DS9 benchmark. Run the benchmark with
`cargo run --locked --release -p xtask --bin xtask -- benchmark --check`.
The contract tests reject documented or CI invocations that omit the binary. Tests
include synthetic executable headers and injected process results to exercise
negative paths; those fixtures are not native-platform qualification receipts.
Actual smoke, corpus and validation commands call the native process runner.

The source architecture gate rejects interpreter source/bytecode and alternate
implementation stacks, interpreter setup in CI and operational documentation
commands that reintroduce a dependency. It checks tracked and nonignored new
files without changing them. CI calls this gate and the Rust tests directly.

Missing compilers or runners mean the affected verification was not executed.
They do not authorize fake passing receipts or prevent writing further Rust.
The validator distinguishes pass, fail, unavailable, blocked and not_run and the
readiness verifier accepts only actual successful native validation. Real beta,
consent, visual review, signing and cross-platform installation remain separate
external validations. No source migration alone sets `ready_for_v1` to true.

## History

The 26 DS9-DS12 development commits are preserved. Their nine interpreter-based
modules and thirteen test files have been replaced by the modules above in new
commits, not hidden by rewriting or force-pushing history. Their useful behavior
is covered in Rust; there is no retained interpreter implementation or fallback.
The validation receipt advances to v2 while agent v2 and document IR contracts
remain unchanged. See RELEASE.md for the receipt migration and acceptance policy.
