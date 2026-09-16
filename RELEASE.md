# Building and reviewing a DocSight release candidate

This guide implements the maintainer side of DS10-DS12. A workflow definition,
passing packaging unit tests or a source archive is not a published release and
is not proof of native behavior. No version is promoted automatically. The
current workspace remains 0.1.4 until maintainers explicitly decide otherwise.

## Prerequisites and scope

Use a complete Git checkout, including `fuzz/`, the locked Rust dependencies,
Rust 1.96.0 with rustfmt and Clippy, and Python 3.11 or later. The maintainer scripts
use only the Python standard library. Building and obtaining dependencies may
need network access; running the packaged DocSight does not. Use one native host
per target from `release/targets.json`; cross-compilation alone is not acceptance.
See INSTALL.md for the configured operating-system baselines and installation.

Keep the existing Rust engine, immutable IR and public agent v2 contract separate
from release automation. The binary self-spawns its sandbox worker; no separate
worker executable or Python interpreter is shipped as a runtime requirement.
The Python tools orchestrate distribution and validation, while `xtask` remains
the authoritative DS9 benchmark implementation. Tooling consolidation into Rust
can be evaluated later without changing the document engine's contracts.

## Automated candidate build

`.github/workflows/release.yml` runs for pull requests or an explicit manual
workflow dispatch. It has `contents: read`, tests each native target, enforces
locked dependencies, builds the binary, collects dependency notices, creates a
ZIP and verifies the extracted executable. Windows applies static CRT flags in
`RUSTFLAGS`; no target-specific setting is allowed to be silently shadowed.

The workflow requires 18 smoke checks per native archive: version, capabilities,
DOCX/PDF inspection, repeated inspection bytes, text, render, an identical diff,
a sandboxed inspection, a typed error and five shell completion generators. The
executable runs without Cargo or Rust on PATH. PNG validation checks structure,
size and CRCs, not visual fidelity to Word or correctness of every rendered pixel.
It then executes the tracked 12-case synthetic corpus and preserves its exact
manifest and receipts. Neither these examples nor their repeats constitute a
broad real-document corpus.

Only after all five native jobs pass does the assembler verify the archive and
smoke-receipt set, collect SHA256SUMS and generate deterministic notes. It uploads
a `docsight-release-candidate` workflow artifact. It does **not** create a GitHub
Release, push a commit or tag, merge a branch, sign a binary or notarize it.
Publishing and credentials require separate maintainer authorization. A failed
job retains available smoke, corpus and benchmark reports for diagnosis.

## Local native build example

The following example is for Linux x64 in a clean source checkout. Use a new
output location instead of overwriting a previous candidate. Windows and macOS
must use their exact native target, runner environment and flags from the
workflow; do not validate an ARM package by executing an Intel package instead.

```sh
export TARGET=x86_64-unknown-linux-gnu
export REVISION="$(git rev-parse HEAD)"
export VERSION="$(python -c 'from scripts.release import workspace_version; print(workspace_version())')"
export RUSTFLAGS='-D warnings'
python -m unittest discover -s scripts/tests -v
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features --target "$TARGET" -- -D warnings
cargo test --locked --workspace --all-features --target "$TARGET"
cargo build --locked --release -p docsight-cli --bin docsight --target "$TARGET"
cargo metadata --locked --format-version 1 --all-features > target/release-metadata.json
python -m scripts.notices --metadata target/release-metadata.json --out target/THIRD_PARTY_NOTICES.md
python -m scripts.release package --binary "target/$TARGET/release/docsight" --target "$TARGET" --revision "$REVISION" --notices target/THIRD_PARTY_NOTICES.md --out dist
python -m scripts.release verify "dist/docsight-$VERSION-$TARGET.zip"
python -m scripts.smoke "dist/docsight-$VERSION-$TARGET.zip" --out "dist/smoke-$TARGET.json"
python -m scripts.corpus run --archive "dist/docsight-$VERSION-$TARGET.zip" --out "dist/corpus-$TARGET.json"
```

The packager rejects wrong executable architecture, unsafe or colliding archive
paths, symlinks, missing resources, corrupted members and inconsistent hashes or
permissions. Archives retain schemas, offline guides, two examples, declared
project license texts and dependency notices. Fixed entry order, timestamps and
modes make packaging deterministic for identical inputs. This is not a claim
that compiling the engine produces reproducible binaries across arbitrary hosts.
Dependency notices include resolved build/test packages and are not a runtime
SBOM or a legal compatibility assessment. Review third-party obligations before
redistribution.

Once all five archives and their smoke receipts are in `dist`:

```sh
python -m scripts.release collect dist
python -m scripts.changelog --revision "$REVISION" --out dist/RELEASE_NOTES.md
```

`--since FULL_ANCESTOR_SHA` limits changelog entries to a validated ancestor range.
Checksums detect corruption, not publisher authenticity. The current pipeline
has no signing or notarization step. Do not disable operating-system security
controls to install an unsigned candidate.

## Full local validation and real evidence

After committing the candidate, run the complete gate collector. The output
must be a new directory; `target/` is ignored by Git.

```sh
python -m scripts.validate --out target/candidate-evidence/validation
```

It attempts all nine gates even if a tool is unavailable: Python tests, Python
syntax, corpus inventory, Cargo format, Clippy, the full Cargo test suite, release
build, DS9 benchmark budgets and Git whitespace validation. It records stdout,
stderr, hashes, exit codes, source revision and clean-tree state before and after.
A missing compiler is `blocked`, not `passed`; a failed test is `failed`, not an
external blocker. Exit code 1 means at least one gate or clean-tree condition did
not pass. The original Rust fuzz procedure in FUZZING.md still needs execution;
a Python harness unit test is not a parser fuzz campaign.

Build an evidence directory with real outputs, not edited success flags:

```text
candidate-evidence/
  validation/validation.json
  validation/*.stdout.log
  validation/*.stderr.log
  docsight-VERSION-TARGET.zip                 (five targets)
  docsight-VERSION-TARGET.zip.sha256          (five targets)
  smoke-TARGET.json                          (five targets)
  corpus-TARGET.json                         (five targets)
  corpus-manifest.json
  beta/observation-*.json
  reviews.json
  reviews/*.md
  before/*.json                             (when fixes need pre-fix evidence)
```

Run BETA.md's campaign with 5-10 independent technical users. For the broad
corpus, prepare an approved manifest of consented real and synthetic cases and
run that same manifest on all five native packages. Keep private source documents
outside Git and shared artifacts. The gate verifies manifest identity and input
hashes without requiring private source files in the evidence directory; the
human review must verify provenance and consent. Historical corpus notes apply
to their original engine and inputs, not automatically to a new candidate.

## Acceptance policy and reviews

`release/readiness-policy.json` initially proposes at least 5 distinct beta
participants, 100 distinct successful DOCX/PDF primary inputs and 25 consented
real inputs in each format. The original plan specifies 5-10 users and a broad
corpus, **not the 100/25 numeric thresholds**. These are explicit initial
engineering defaults requiring maintainer review, not approved product targets.
Change them only through a reviewed commit with rationale; do not lower them to
make a failing candidate appear qualified. Repeating one document does not
increase the distinct-document count. A diff reference alone does not count as
another successfully inspected primary document.

`reviews.json` contains `schema: docsight.release-reviews/v1`, `version`, full
`revision`, the SHA-256 of the reviewed policy file as `policy_sha256`, and a
`reviews` object with exactly these keys: `policy`, `installation`,
`behavior-and-json`, `render-and-diff`, `security-and-fuzzing`, and
`beta-participation-and-triage`. Each entry contains `approved`, a stable
`reviewer` identifier and `evidence: {"file": "reviews/name.md", "sha256": "..."}`.
Each real review note must contain the procedure, observed results and limits;
a checkbox or fabricated identity is not a review. The verifier checks artifact
consistency and recorded attestations; it cannot establish a person's identity,
independence or the truth of an unsigned attestation.

Review installation on clean machines without Rust; CLI/JSON compatibility and
diagnostics; rendering and changed-document diffs; adversarial documents, worker
resource limits and actual fuzz results; participant independence and all beta
issues. In particular, a small smoke pass does not demonstrate that crashes are
rare or that general document rendering is faithful.

Run the acceptance check against the exact candidate revision:

```sh
python -m scripts.readiness --evidence target/candidate-evidence --revision "$REVISION" --out target/candidate-readiness.json
```

The output evaluates seven criteria and returns exit code 1 while any fail.
`ready_for_v1: false` must remain visible. Resolving every recorded beta bug
requires both pre-fix failure and post-fix corpus evidence. An empty beta issue
register can satisfy its narrow consistency check but cannot satisfy missing
participants, reviews, corpus or native packages.

## Current V1 blockers

`release/known-gaps.json` preserves unresolved multi-section DOCX geometry,
line-level pagination and persistent content-addressed cache as blocking. A
per-invocation ingestion-limit override remains deferred until a demonstrated
real case, matching the plan's caveat. Updating a register is not implementation:
close a gap only with code, regression tests and actual native validation.

The versioned maintainer schemas are in `schemas/tooling/v1/evidence.json`.
They describe beta observations, release manifests, smoke receipts, validation,
corpus manifests/results, readiness and typed tooling errors. Runtime validators
also enforce cross-file identities and semantic constraints. They do not replace
or alter `docsight.agent/v2` or the Document IR schema.
