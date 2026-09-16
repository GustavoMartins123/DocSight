# Running a real DocSight beta

This is the DS11 operating procedure, not evidence that a beta has happened.
Recruit 5-10 technical users who run the same reviewed candidate on their own
machines. Synthetic test participants and maintainer-generated reports do not
count as independent users. Keep the consent record and the association between
people and `beta-001` style identifiers outside the repository.

## Candidate and consent

Use the native packages built under RELEASE.md. Record the full source revision,
archive SHA-256 and platform. Test installation without Rust, a normal inspection,
a confusing command, rendering and a meaningful two-document diff. Observe which
features each person actually needs; do not require them to report artificial
successes. The default corpus in the source tree is synthetic and is not a beta.

The collector runs only when explicitly invoked. It does not upload anything,
start a service, watch directories or contact a server. It requires a source
checkout and Python 3.11 or later; the DocSight executable itself does not require
Python. A maintainer can assist a participant instead of asking every participant
to install the development tools. Obtain consent before running on any document
and again before sharing a resulting report or reproducer.

## Collect a local observation

Run these commands from the source checkout. Replace `ARCHIVE.zip` with the
verified native package for the participant's platform, and use a new output file
for each observation:

```sh
python -m scripts.beta collect --archive ARCHIVE.zip --participant beta-001 --operation inspect --experience clear --document /private/sample.docx --out beta-reports/observation-001.json
python -m scripts.beta collect --archive ARCHIVE.zip --participant beta-001 --operation diff --experience confusing --document /private/before.docx --reference /private/after.docx --out beta-reports/observation-002.json
python -m scripts.beta aggregate --reports beta-reports --out beta-summary.json
```

Allowed operations are `capabilities`, `inspect`, `overview`, `text`, `render` and
`diff`. `capabilities` takes no document; `diff` requires an explicit reference.
Experience is one of `clear`, `confusing` or `blocked`, selected by the user.
A known PDF password can be supplied with `--password-file /private/password.txt`.
Protect that file with the operating system's access controls and delete it when
no longer required. The collector passes the path, not the password value, to
DocSight. Do not put a password into a command argument, issue or report.

Reports include the candidate identity, pseudonymous participant, selected
operation, perceived experience, outcome, exit code, elapsed milliseconds,
byte counts and documented diagnostic codes. They exclude document names, paths,
text, raw command lines, password values and free-form error messages. Unknown
diagnostic values are counted, not copied. Document hashing is disabled unless
`--include-document-digest` is explicitly selected; even a hash may identify a
known document. Review metadata before sharing it. Local files are created
without overwriting existing ones; enforce appropriate directory ACLs on Windows.

Each command uses `--sandbox`, a 45-second outer deadline and a 256 KiB captured
output budget. Reports distinguish `success`, typed `error`, `crash`, `timeout`,
`output_limit` and `invalid_protocol`. Collector exit code 0 means an observation
was written, **not** that DocSight succeeded. Read `outcome`. Exit code 2 means the
collector could not produce a valid report. These collection limits describe
this beta harness, not the full engine's capabilities or performance limits.

Aggregation rejects mixed revisions and duplicate reports, including a copied
report with different JSON whitespace. It counts unique pseudonyms, not real
identities; a human must verify the independent participants. Timing summaries
are observational percentiles, not DS9 benchmark results or hardware-normalized
performance comparisons. Preserve the individual reports alongside the summary.

## Triage without collecting private content by default

Use the beta issue form to record the task, expected versus observed behavior,
public diagnostic codes, platform and candidate. Never automatically attach a
failed file. First minimize the reproducer, remove confidential content and
obtain permission to share it. A necessary private reproducer stays in a
restricted local corpus, never in Git, public issues or CI artifacts. Reviewers
must verify that entries labeled `consented-real` actually have consent.

For a confusing command, record the intended task and sanitized invocation. For
an incorrect render or diff, record the visual discrepancy and reviewed expected
behavior. No crash rate or reliability claim can be inferred from an empty issue
register, a small synthetic corpus or a few successful demonstrations.

## A regression before every beta fix

1. Add a minimal case to a reviewed corpus manifest. Bind each input to its
   SHA-256 and record assertions for the expected correct result. A `diff` case
   accepts `reference: {"file": "relative/after.docx", "sha256": "..."}`; omitting
   it intentionally means a self-diff, not an inferred second document.
2. Execute that case against the pre-fix package and retain the failing corpus
   report. A missing tool, missing file or invented report is not a reproduced
   bug. Protect the original input from modification during execution.
3. Add the narrow engine unit/integration test, implement the fix, rerun the
   focused tests and relevant full suites, review the diff and commit the fix.
4. Execute the same hashed inputs against the candidate on every target. Link
   both the pre-fix failure and the passing regression in `release/beta-issues.json`.

The manifest requires `schema: docsight.corpus/v1` and a `cases` array. Each case
has `id`, `file`, `sha256`, `origin`, `format`, `operation` and `expected`.
The expectation contains `exit_code`, sorted `diagnostic_codes`, scalar
`pointer_equals` assertions and `repeat` (1-3). Use the tracked
`release/corpus.json` as a concrete example. Paths must remain under the supplied
root. A private corpus can be executed without copying it into the repository:

```sh
python -m scripts.corpus validate --root /private/corpus --manifest /private/corpus/manifest.json
python -m scripts.corpus run --archive ARCHIVE.zip --root /private/corpus --manifest /private/corpus/manifest.json --out /private/corpus/report.json
```

`validate` checks inventory and hashes only; its output explicitly says
`executed: false`. `run` executes the engine and returns exit code 1 for a failed
campaign. Keep the public assertion values themselves free of sensitive text.

An issue record contains `id`, `severity` (`blocker`, `high`, `medium`, `low`),
`status` (`open`, `resolved`, `deferred`), `regression_case` and `before_evidence`.
For an unresolved issue the last two fields may be null. Resolving an issue
requires the passing case identifier and an evidence reference such as
`{"file": "before/beta-001.json", "sha256": "..."}`. The referenced report must
come from a different pre-fix revision and show an actual failed attempt on the
same primary and reference input hashes. High-severity open or deferred issues
block V1. Lower-severity deferral needs an explicit triage decision in the review.

## Completion is human and technical

The beta is complete only after real participants have tried the candidate,
observations and failures have been triaged, fixes have regression coverage and
the release review records the limitations. RELEASE.md describes the evidence
layout and gate. The initial empty issue register means no issues are recorded;
it must never be presented as a successful beta campaign.
