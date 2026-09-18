# Building and reviewing a DocSight release candidate

This guide implements the maintainer side of DS10-DS12 and DS16. Source code, packaging
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
Failed jobs retain available receipts for diagnosis. Each native job also
records a signature receipt, a provenance job records one provenance receipt
per archive, and an installation job repeats INSTALL.md on a fresh runner of
every target (see Authenticated distribution). Signing, notarization and
attestation run only for the targets the committed distribution policy marks as
required. The workflow uploads candidate artifacts only: no push, tag, merge or
GitHub Release is performed.

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
| `actions/attest-build-provenance` | `v4.2.2` | `4d101475d8b20a2381f78447822ac1eab6504dd8` |
| `dtolnay/rust-toolchain` | `master` | `02cb101ec7c40f2c49e1d9714d64511d8e1b74de` |

## Authenticated distribution

Checksums detect corruption; they do not tell a user who published an archive.
Authenticated distribution adds three independent proofs, each declared in
`release/distribution-policy.json` and verified from the finished archive:

| Proof | Targets | Mechanism | Verified by |
| --- | --- | --- | --- |
| Code signature | Windows x64 | Authenticode with a timestamp, signer subject pinned in the policy | `Get-AuthenticodeSignature` on a Windows host |
| Code signature and notarization | macOS Intel and Apple Silicon | Developer ID Application with hardened runtime and a timestamp, Team ID pinned in the policy, notarized by Apple | `codesign --verify --strict`, `codesign --display` and `spctl --assess --type install` on a macOS host |
| Build provenance | all five | GitHub artifact attestation (SLSA provenance v1, signed through Sigstore) bound to this repository, the release workflow and the source commit | `gh attestation verify` |

Linux has no platform code signature; its authenticity rests on provenance.
Each policy entry is `required`, `pending-credential` or, for Linux signing
only, `not-applicable`; provenance is `required` or
`pending-attestation-support`. A required entry names its publisher (the
certificate subject or the Team ID) and a pending entry names none, so a
publisher can only be introduced by a reviewed commit. The workflow reads the
same file: `cargo xtask release configuration` adds each target's signing status
to the matrix and exports the provenance status, and the signing, notarization
and attestation steps run only where the policy says `required`. There is no
path that signs because a secret happens to exist.

```sh
cargo xtask release signature "dist/docsight-$VERSION-$TARGET.zip" --out "dist/signature-$TARGET.json"
cargo xtask release provenance "dist/docsight-$VERSION-$TARGET.zip" --out "dist/provenance-$TARGET.json"
```

`release signature` runs on the target's native host. It extracts the verified
archive, classifies the signature embedded in both executables from their
Mach-O or PE structures (`not-applicable`, `absent`, `ad-hoc` or `cms`, plus the
hardened runtime flag) and, for a required target, asks the operating system to
verify it. The receipt (`docsight.release-signature/v1`) records the status
`verified`, `pending-credential`, `not-applicable` or `failed` with the first
error code, such as `SIGNATURE_MISSING`, `SIGNATURE_INVALID`,
`PUBLISHER_MISMATCH`, `SIGNATURE_NOT_TIMESTAMPED`, `HARDENED_RUNTIME_MISSING`,
`NOTARIZATION_MISSING` or `UNEXPECTED_SIGNATURE` for a signature the policy does
not declare. Apple Silicon linkers sign every executable ad hoc; an ad-hoc
signature names no publisher and is not a Developer ID signature. The command
exits 1 only for `failed`.

`release provenance` runs `gh attestation verify` with the policy repository,
signer workflow, the archive's source commit, the SLSA provenance predicate and
`--deny-self-hosted-runners`, then checks the returned certificate fields again.
It needs `GH_TOKEN`. Its receipt (`docsight.release-provenance/v1`) records the
signer, source commit and ref, runner environment, workflow run and transparency
log entries of each attestation.

`release collect` rejects a candidate whose signature receipts do not describe
its archives, differ from the executables actually packaged, or report a status
other than the one the policy declares. The readiness criterion
`authenticated-distribution` additionally requires every provenance receipt and
fails with `SIGNING_CREDENTIAL_PENDING` or `PROVENANCE_PENDING` while any entry
is pending, so an unsigned candidate cannot be declared ready.

The `install` job downloads the candidate on a fresh runner of each target,
verifies the attestation when provenance is required, checks the checksum with
the operating system's own tool, extracts the archive and runs the INSTALL.md
commands without Rust or a checkout. On Linux and macOS it runs them with an
empty environment and an empty home directory, and fails if DocSight writes
anything there.

### Current status and required credentials

Today every signing entry is `pending-credential` and provenance is
`pending-attestation-support`. No certificate is present, the repository is
private, and GitHub artifact attestations require a public repository or
GitHub Enterprise Cloud. Candidates are therefore unsigned, and readiness
reports it. To enable each proof:

1. macOS: enrol in the Apple Developer Program, create a Developer ID
   Application certificate and an App Store Connect API key for the notary
   service, store the secrets below, then commit the policy entries for both
   macOS targets as `required` with the Team ID as `publisher`.
2. Windows: choose a code signing provider whose keys live in a hardware
   security module (the CA/Browser Forum no longer permits exportable keys).
   The workflow has no Windows signing step yet because the provider decides how
   `signtool` reaches the key. Add that step before packaging, then commit the
   Windows entry as `required` with the certificate subject exactly as
   `Get-AuthenticodeSignature` reports it.
3. Provenance: make the repository public or move it to GitHub Enterprise
   Cloud, then commit `provenance.status` as `required`.

| Secret | Used by | Content |
| --- | --- | --- |
| `APPLE_DEVELOPER_ID_CERTIFICATE_P12` | macOS signing | Base64 of the Developer ID Application certificate and private key exported as PKCS #12 |
| `APPLE_DEVELOPER_ID_CERTIFICATE_PASSWORD` | macOS signing | Password of that PKCS #12 file |
| `APPLE_DEVELOPER_ID_IDENTITY` | macOS signing | Certificate name passed to `codesign --sign`, such as `Developer ID Application: NAME (TEAMID)` |
| `APPLE_NOTARY_API_KEY_P8` | macOS notarization | Content of the App Store Connect API private key (`.p8`) |
| `APPLE_NOTARY_API_KEY_ID` | macOS notarization | Key identifier of that API key |
| `APPLE_NOTARY_API_ISSUER_ID` | macOS notarization | Issuer identifier of the App Store Connect team |

Provenance needs no secret: the provenance job alone receives `id-token: write`
and `attestations: write`, and GitHub issues its short-lived signing identity.
The macOS secrets reach only the two steps that sign and notarize. A contract
test fails if a workflow uses a secret this table does not document, if a
signing or attestation step runs without its policy condition, or if another job
receives elevated permissions. None of these credentials authorizes a
publication: releases remain a separate, explicitly authorized action.

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
cargo xtask release signature "dist/docsight-$VERSION-$TARGET.zip" --out "dist/signature-$TARGET.json"
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

Once all five archives, sidecars, smoke and signature receipts are in `dist`:

```sh
cargo xtask release collect dist
cargo xtask changelog --revision "$REVISION" --out dist/RELEASE_NOTES.md
```

`--since FULL_ANCESTOR_SHA` restricts notes to a verified ancestor range. Both
range endpoints are full commit identifiers, never arbitrary shell expressions.
Checksums detect corruption, not publisher identity. Signing, notarization and
attestation follow the distribution policy above; final publication requires
explicit authorization.
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
  signature-TARGET.json                      (five targets)
  provenance-TARGET.json                     (five targets)
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
