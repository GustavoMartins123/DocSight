# Building and reviewing a DocSight release candidate

This guide implements the maintainer side of DS10-DS12. Source code, packaging
unit tests and workflow definitions are not evidence of a published release,
native behavior, a completed beta or visual fidelity. The current workspace
remains 0.1.4. No command here promotes a version or publishes a release.

## Rust-only maintenance

Use a complete Git checkout including `fuzz/`, the locked dependencies and Rust
1.96.0 with rustfmt and Clippy. All permanent maintenance logic lives in the
existing `xtask` crate. The workspace alias `cargo xtask` runs it with the lockfile
enforced. Build and dependency acquisition may require network access; running
the packaged application is offline. There is no interpreter dependency or
fallback implementation.

Use one native host for each target in `release/targets.json`. Cross-compilation
alone is not native acceptance. INSTALL.md lists configured baselines, not a
claim that every baseline has been tested. The packages include both `docsight`
and `docsight-worker`, with `.exe` on Windows. The CLI retains its existing
self-spawn isolation path; shipping the worker does not change the agent v2
protocol or immutable IR contracts.

## Automated candidates

`.github/workflows/release.yml` runs on pull requests or explicit workflow
dispatch. It has `contents: read`. Each native job runs the Rust tooling tests,
architecture gate, formatter, Clippy, workspace tests and workspace release
build with locked dependencies. Windows keeps static CRT flags in `RUSTFLAGS`.
The canonical Linux x64 host also enforces DS9 benchmark budgets.

The native packager validates both executable architectures, collects notices,
creates a bounded deterministic ZIP and verifies it. Smoke execution then uses
a freshly extracted package with an isolated environment and no Rust on PATH.
Eighteen checks cover version, capabilities, DOCX/PDF inspection, repeated
inspection bytes, text, render, identical diff, sandbox inspection, typed error
and five completion shells. PNG validation checks dimensions, structure, CRCs
and decoding; it is not a human review of rendering fidelity.

Each job executes the 12-case synthetic corpus and retains its manifest and
receipts. Repeats and these small examples do not constitute broad real-document
coverage. Only after all five native jobs succeed does assembly verify archives,
sidecars and smoke receipts, collect `SHA256SUMS` and generate release notes.
Failed jobs retain available receipts for diagnosis. The workflow uploads a
candidate artifact only: no push, tag, merge, GitHub Release, signing or
notarization is performed.

## Pinned CI actions

Every workflow step that uses an action names a full commit, not a tag or
branch that its owner could move. Checkouts do not persist the job token in the
working copy. A contract test rejects any workflow reference that differs from
this register, and any registered action no workflow uses. To upgrade an
action, resolve the new release tag to its commit and change the workflows and
this table in the same commit.

| Action | Upstream reference | Commit |
| --- | --- | --- |
| `actions/checkout` | `v4.4.0` | `11d5960a326750d5838078e36cf38b85af677262` |
| `actions/upload-artifact` | `v4.6.2` | `ea165f8d65b6e75b540449e92b4886f43607fa02` |
| `actions/download-artifact` | `v4.3.0` | `d3f86a106a0bac45b974a628896c90dbdf5c8093` |
| `dtolnay/rust-toolchain` | `master` | `02cb101ec7c40f2c49e1d9714d64511d8e1b74de` |

## Local native build example

This example is for Linux x64. `VERSION=0.1.4` matches the current Cargo workspace;
verify the value using `cargo xtask release version` after a version change.
Use fresh output locations. Other targets must use their actual native host,
filename suffixes and environment from the workflow.

```sh
export TARGET=x86_64-unknown-linux-gnu
export REVISION="$(git rev-parse HEAD)"
export VERSION=0.1.4
export RUSTFLAGS='-D warnings'
cargo test --locked -p xtask --all-targets
cargo xtask rust-only
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features --target "$TARGET" -- -D warnings
cargo test --locked --workspace --all-features --target "$TARGET"
cargo build --locked --release --workspace --all-features --target "$TARGET"
cargo metadata --locked --format-version 1 --all-features --filter-platform "$TARGET" > target/release-metadata.json
cargo xtask notices --metadata target/release-metadata.json --out target/THIRD_PARTY_NOTICES.md
cargo xtask release package --binary "target/$TARGET/release/docsight" --worker "target/$TARGET/release/docsight-worker" --target "$TARGET" --revision "$REVISION" --notices target/THIRD_PARTY_NOTICES.md --out dist
cargo xtask release verify "dist/docsight-$VERSION-$TARGET.zip"
cargo xtask smoke "dist/docsight-$VERSION-$TARGET.zip" --out "dist/smoke-$TARGET.json"
cargo xtask corpus run --archive "dist/docsight-$VERSION-$TARGET.zip" --out "dist/corpus-$TARGET.json"
```

The packager rejects incorrect executable headers, missing workers or resources,
unsafe or case-colliding member paths, symlinks, encrypted members, excessive
expanded sizes, corrupt members, unknown members and inconsistent hashes or
permissions. It retains all schemas, offline guides, two examples, project
license texts and third-party notices. Order, timestamps and modes are fixed
for identical inputs. This does not claim reproducible compiler outputs across
arbitrary hosts. Notices include resolved build/test dependencies as well as
application dependencies; they are not a runtime-only SBOM or legal assessment.
Metadata is filtered to the packaged target, so each archive lists the packages
resolved for that platform. Unfiltered metadata also resolves packages for
unrelated platforms, and a package without distributable license text fails
with `MISSING_LICENSE_TEXT` instead of producing incomplete notices.

Once all five archives, sidecars and smoke receipts are in `dist`:

```sh
cargo xtask release collect dist
cargo xtask changelog --revision "$REVISION" --out dist/RELEASE_NOTES.md
```

`--since FULL_ANCESTOR_SHA` restricts notes to a verified ancestor range. Both
range endpoints are full commit identifiers, never arbitrary shell expressions.
Checksums detect corruption, not publisher identity. Signing, notarization and
final publication require separate credentials and explicit authorization.
Do not disable operating-system security controls to install an unsigned build.

## Full native validation

After committing the candidate, collect all gates into a new directory under
the Git-ignored `target/` tree:

```sh
cargo xtask validate --out target/candidate-evidence/validation
```

The nine checks are Rust tooling tests, the Rust-only architecture audit, corpus
inventory, Cargo format, Clippy, workspace tests, release build, DS9 budgets and
Git whitespace validation. Every gate is attempted even when an earlier gate
fails. Logs, hashes, elapsed time, real exit codes, source revision and clean-tree
state before and after are retained. The `docsight.validation/v2` status values
are `pass`, `fail`, `unavailable`, `blocked` and `not_run`.

A missing executable is `unavailable` with a null exit code; an executed failing
test is `fail`. A process-start or access failure is `blocked`; an unsupported
host may be `not_run`. Neither is a successful check. The command returns 1 if a
gate, clean-tree or revision condition fails, and 2 for invalid inputs or failure
to construct a report. If Cargo and the compiled xtask are both unavailable,
no validator ran: record NOT EXECUTED externally, not a fabricated validator
receipt. Development can continue independently of this execution status.

`cargo xtask corpus validate` only checks inventory and hashes. Its receipt says
`executed: false`. `cargo xtask corpus run` actually executes the candidate.
The Rust fuzz procedures in FUZZING.md remain necessary; tooling unit tests do
not substitute for native parser fuzz campaigns.

## Candidate-bound evidence

Prepare the following directory from actual outputs, never edited success flags:

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
  before/*.json                             (pre-fix regression evidence)
```

Run BETA.md's procedure with 5-10 independent technical participants. Execute the
same reviewed broad corpus manifest on all five native candidates. Keep source
documents and consent records outside Git and shared artifacts. Readiness checks
receipt identity and hashes without copying private corpus inputs; reviewers
must verify provenance and consent. Historical results do not automatically
qualify a different source revision or set of inputs.

## Policy and real reviews

`release/readiness-policy.json` proposes five distinct participants, 100 distinct
successful DOCX/PDF primary inputs and 25 consented real inputs per format. The
original plan specifies 5-10 users and a broad corpus, not the numeric 100/25
thresholds. These numbers remain engineering proposals requiring policy review,
not automatically approved product criteria. The policy records this as
`threshold_status: engineering-proposal`, and the readiness report repeats it in
`policy_thresholds`. The report shows `approved` only when a reviewed commit sets
`threshold_status: approved` and the recorded `policy` review for that exact
policy digest is approved; until then `ready_for_v1` stays false even if every
criterion passes. Changes require a reviewed commit and rationale, not silent
relaxation to make a candidate appear ready. Repeats and diff-only references do
not inflate the distinct primary-input count.

## Quality measurement by document class

Passing corpus cases show that the engine behaved as a reviewed expectation says
for those inputs. They do not show how well it handles other documents, so
quality is measured per document class instead of being extrapolated from a few
synthetic samples to every DOCX or PDF.

`release/document-classes.json` declares the classes, such as `docx-tabular`,
`pdf-structured` or `unsupported-format`, with a format, a complexity tier
(`simple`, `moderate`, `complex` or `adversarial`) and the minimum properties an
`inspect` result must show for a document of the class. Every corpus case names
its class, and only an adversarial class may expect a rejection.

```sh
cargo xtask quality measure --archive ARCHIVE.zip --out quality.json
cargo xtask quality compare --baseline previous-quality.json --candidate quality.json
```

`measure` runs the packaged engine in its sandbox twice per document and reports
each metric with a status and the basis it was judged on:

| Basis | Metrics | Meaning |
| --- | --- | --- |
| `engine-invariant` | `determinism`, `classification`, `render_geometry`, `diff_identity` | Properties the engine must hold for any document: repeated runs are byte-identical, the document shows its class signals, the page-one raster covers the page within one pixel, and a document compared with itself has no changes. |
| `corpus-expectation` | `rejection` | An adversarial input fails with the exit code and diagnostic the reviewed manifest declares. |
| `reviewed-ground-truth` | `structure`, `text`, `geometry`, `diagnostics`, `render`, `diff` | The output agrees with facts a person checked against the document. |
| `unreviewed-proposal` | the same metrics | The output matches a record proposed from an earlier engine run. This detects drift but is not evidence of correctness. |

A failure on the first three bases fails the measurement; drift from an
unreviewed proposal is reported without failing it. Each class summary states
its document count, how many documents are synthetic or consented real, how many
have reviewed ground truth and which evidence the class rests on:
`reviewed-ground-truth`, `partially-reviewed-ground-truth`,
`engine-consistency-only`, `corpus-expectation` or `no-documents`. The report
carries a fixed statement that it makes no claim about other documents.
`compare` lists every metric that stopped passing, every document that stopped
being measurable or disappeared, and exits 1 when there is any regression.

### Ground truth and human review

Ground truth records live in a register directory, `release/ground-truth` by
default, one `<document-sha256>.json` per document. The register holds no
records today, so every class currently rests on engine consistency or on
corpus expectations only.

```sh
cargo xtask quality prepare --archive ARCHIVE.zip --document doc.docx --class docx-tabular --out release/ground-truth/<sha256>.json
cargo xtask quality validate
```

`prepare` records the facts the engine reports (structure counts, a digest of
the block text, page-one geometry, diagnostics, the page-one PNG digest at 72
dpi and optional diff summaries against `--reference` documents) and always
writes `review.status: unreviewed`. A reviewer then checks each value against
the document itself, corrects any value the engine got wrong, writes a note
describing the procedure, observations and limitations, and records
`status: reviewed`, a stable `reviewer` identifier and the note's path and
SHA-256 relative to the register. `validate` and `measure` reject a reviewed
record whose note is missing, changed or shorter than 120 bytes, and an
unreviewed record that carries reviewer data. Tooling never marks a record
reviewed, and an unsigned note cannot establish the reviewer's identity. Keep
records for private documents outside the repository and pass the register with
`--ground-truth`.

`reviews.json` must contain `schema: docsight.release-reviews/v1`, `version`, full
`revision`, `policy_sha256`, and exactly six review categories in `reviews`:
`policy`, `installation`, `behavior-and-json`, `render-and-diff`,
`security-and-fuzzing`, and `beta-participation-and-triage`. Each entry has
`approved`, a stable `reviewer` identifier, and an evidence reference such as
`{"file":"reviews/installation.md","sha256":"..."}`. The hash binds an actual
nontrivial review note describing procedure, observations and limitations.
Unit-test participants and synthetic positive receipts are not human reviews.
An unsigned note cannot establish identity, independence or the truth of a claim.

Review clean-machine installation, protocol compatibility and diagnostics,
rendering and changed-document diff, adversarial inputs and worker limits,
actual fuzz results, independent participants and regression triage. An empty
issue register is not evidence of low crash rates or successful beta completion.
Resolved beta issues require a genuine failing pre-fix corpus attempt on the
same primary and reference hashes and a passing candidate regression. Input
mutation, missing files or missing tools do not prove a reproduced engine bug.

```sh
cargo xtask readiness --evidence target/candidate-evidence --revision "$REVISION" --out target/candidate-readiness.json
```

The eight criteria cover workspace validation, five native packages,
authenticated distribution, beta observations, broad corpus, manual reviews,
regressions and known V1 gaps.
Any failed criterion keeps `ready_for_v1: false` and yields exit 1. Structural
consistency of these artifacts is not independent certification.

## Remaining engine gaps and schema transition

`release/known-gaps.json` records multi-section DOCX geometry and line-level
pagination as resolved by the DS13 layout engine, and the persistent
content-addressed document IR cache as resolved. No v1-blocking engine
gap remains open, so the known-gaps criterion no longer reports
`OPEN_V1_PRODUCT_GAPS`; the other readiness criteria still require their own
evidence. A resolved entry records an implemented and tested engine feature; it
is not a claim of fidelity to Word, whose remaining layout limitations are
listed in BACKLOG.md and PRODUCT_SCOPE.md, nor a claim that every command result
is cached, which is limited to the document IR as described in
PRODUCT_SCOPE.md.

The current maintainer contract registry is `schemas/tooling/v2/evidence.json`.
The validation receipt advances from v1 to v2 because native gate names, status
values and null unavailable exit codes changed. Readiness requires new native
validation receipts; historical reports must not be relabeled as v2. Other
artifact schema identifiers remain v1. Public `docsight.agent/v2` and IR contracts
are unchanged. TOOLING.md maps commands, modules, tests and operational limits.
